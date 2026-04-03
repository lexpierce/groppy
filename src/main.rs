//! groppy - A parallel Git repository updater written in Rust
//!
//! This tool scans directories for Git repositories and updates them concurrently
//! using gitoxide (gix) for fetch operations and rayon for parallelism. It mirrors
//! the functionality of goppy (the Go version) with the same CLI interface and TUI.
//!
//! # Usage
//!
//! ```sh
//! groppy                      # Update repos in current directory
//! groppy [dir1] [dir2]        # Update repos in specified directories
//! groppy -v                   # Verbose output (show unchanged repos)
//! groppy -j 8                 # Use 8 parallel jobs
//! ```

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use crossterm::style::{Color, Stylize};
use gix::bstr::ByteSlice;

// Catppuccin Mocha color palette constants
// These define the RGB values used for terminal output styling

/// Green color for successful repo update messages
const COLOR_GREEN: Color = Color::Rgb {
    r: 166,
    g: 227,
    b: 161,
};

/// Red color for failed repo update messages
const COLOR_RED: Color = Color::Rgb {
    r: 243,
    g: 139,
    b: 168,
};

/// Muted gray color for low-priority text like summaries
const COLOR_SUBTEXT: Color = Color::Rgb {
    r: 108,
    g: 112,
    b: 134,
};

/// Array of Catppuccin Mocha colors the spinner cycles through.
/// The spinner changes color every 3 ticks for a smooth rainbow effect.
const SPINNER_COLORS: &[Color] = &[
    Color::Rgb {
        r: 137,
        g: 180,
        b: 250,
    }, // blue
    Color::Rgb {
        r: 203,
        g: 166,
        b: 247,
    }, // mauve
    Color::Rgb {
        r: 250,
        g: 179,
        b: 135,
    }, //  peach
    Color::Rgb {
        r: 249,
        g: 226,
        b: 175,
    }, // yellow
    Color::Rgb {
        r: 148,
        g: 226,
        b: 213,
    }, // teal
];

/// Braille-based spinner animation frames (10 frames for a smooth rotation)
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Command-line interface definition using clap derive macros.
/// Accepts optional directories, job count, and verbose flag.
#[derive(Parser)]
#[command(name = "groppy", about = "Parallel Git repository updater (Rust + gitoxide)", version)]
struct Cli {
    /// Directories to scan for Git repositories (defaults to current directory)
    directories: Vec<PathBuf>,

    /// Number of parallel jobs for concurrent repo updates
    #[arg(short = 'j', long = "jobs", default_value_t = 4)]
    jobs: usize,

    /// Whether to show verbose output including unchanged repos
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,
}

/// Represents the outcome of updating a single Git repository.
/// Contains all information needed to display the result to the user.
struct RepoStatus {
    path: PathBuf,       //  Absolute filesystem path to the repository
    success: bool,       // Whether the update operation succeeded
    message: String,     // Human-readable description of what happened
    files_changed: usize, // Number of files modified by the update
}

/// Entry point: parses CLI args, discovers repos, runs parallel updates, and prints summary.
///
/// The overall flow is:
///   1. Parse CLI arguments
///   2. Canonicalize and deduplicate directory paths
///   3. Discover Git repositories in those directories
///   4. Spawn a spinner thread for visual feedback
///   5. Update all repositories in parallel using a rayon thread pool
///   6. Print a summary of results
fn main() -> Result<()> {
    let cli = Cli::parse();

    // Default to current directory if no directories specified
    let dirs: Vec<PathBuf> = if cli.directories.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        cli.directories.clone()
    };

    // Canonicalize paths to absolute form and remove any that don't exist
    let dirs: Vec<PathBuf> = dirs
        .into_iter()
        .filter_map(|d| std::fs::canonicalize(d).ok())
        .collect();
    let dirs = unique_ordered(dirs); // Remove duplicate directories

    // Discover all git repositories in the provided directories
    let (repos, scan_warnings) = find_git_repositories(&dirs);
    if cli.verbose {
        for w in &scan_warnings {
            eprintln!("{}", format!("  warning: {w}").with(COLOR_SUBTEXT));
        }
    }
    let total = repos.len();
    let start = Instant::now(); //  Start timing the entire update process

    // Shared atomic counters for thread-safe progress tracking
    let completed = Arc::new(AtomicUsize::new(0));
    let succeeded = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let stop_spinner = Arc::new(AtomicBool::new(false));
    let output_lock = Arc::new(Mutex::new(())); // Prevents interleaved output lines

    // Spawn the spinner animation on a dedicated thread
    let spinner_stop = stop_spinner.clone();
    let spinner_completed = completed.clone();
    let spinner_total = total;
    let spinner_lock = output_lock.clone();
    let spinner_handle = std::thread::spawn(move || {
        run_spinner(spinner_stop, spinner_completed, spinner_total, spinner_lock);
    });

    //  Determine actual job count (0 means use all available CPUs)
    let jobs = if cli.jobs == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    } else {
        cli.jobs
    };

    // Build a rayon thread pool with the requested number of threads
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()?;

    // Process all repositories in parallel within the thread pool scope
    pool.scope(|s| {
        for repo_path in &repos {
            let completed = completed.clone();
            let succeeded = succeeded.clone();
            let failed = failed.clone();
            let output_lock = output_lock.clone();
            let verbose = cli.verbose;

            s.spawn(move |_| {
                // Update the repository and record the result
                let status = update_repository(repo_path);

                // Failures always print; unchanged-success lines respect verbose.
                // The visibility check is explicit here so failures can never be
                // accidentally silenced by a change inside format_line.
                if !status.success || status.files_changed > 0 || verbose {
                    let _lock = output_lock.lock().unwrap();
                    eprint!("\r\x1b[K");
                    println!("{}", format_line(&status));
                }

                // Atomically update progress counters
                completed.fetch_add(1, Ordering::Relaxed);
                if status.success {
                    succeeded.fetch_add(1, Ordering::Relaxed);
                } else {
                    failed.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });

    // Stop the spinner thread and wait for it to finish
    stop_spinner.store(true, Ordering::Release);
    let _ = spinner_handle.join();

    eprint!("\r\x1b[K"); // Clear the final spinner line
    eprint!("\x1b]9;4;0;0\x07"); // Clear OSC 9;4 terminal progress indicator

    // Load final counter values for the summary
    let completed = completed.load(Ordering::Relaxed);
    let succeeded = succeeded.load(Ordering::Relaxed);
    let failed_count = failed.load(Ordering::Relaxed);
    let elapsed = start.elapsed();

    // Print the summary line in muted gray
    println!();
    let summary = format!(
        "repos: {} total | {} done | {} ok | {} fail | jobs: {} | elapsed: {}s",
        total,
        completed,
        succeeded,
        failed_count,
        jobs,
        elapsed.as_secs()
    );
    println!("{}", summary.with(COLOR_SUBTEXT));

    // Exit with error code 1 if any repositories failed
    if failed_count > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// Runs the color-cycling spinner animation on a dedicated thread.
///
/// Displays a braille spinner character that cycles through Catppuccin colors,
/// along with a progress counter showing completed/total repos. Also emits
/// OSC 9;4 escape sequences for terminal tab progress indicators.
///
/// Runs at ~12.5fps (80ms per frame) until the stop flag is set.
fn run_spinner(
    stop: Arc<AtomicBool>,
    completed: Arc<AtomicUsize>,
    total: usize,
    output_lock: Arc<Mutex<()>>,
) {
    let mut frame = 0usize;
    let mut color_idx = 0usize;
    let mut tick = 0usize;

    while !stop.load(Ordering::Acquire) {
        let current = completed.load(Ordering::Relaxed);

        let progress_percent = if total > 0 { (current * 100) / total } else { 0 };
        let spinner_char = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
        let color = SPINNER_COLORS[color_idx % SPINNER_COLORS.len()];

        {
            let _lock = output_lock.lock().unwrap();
            eprint!("\x1b]9;4;1;{progress_percent}\x07");
            eprint!(
                "\r\x1b[K{} Updating repositories... ({}/{})",
                spinner_char.to_string().with(color),
                current,
                total
            );
        }
        let _ = std::io::stderr().flush();

        frame += 1;
        tick += 1;
        if tick.is_multiple_of(3) {
            color_idx += 1;
        }

        std::thread::sleep(Duration::from_millis(80));
    }
}

/// Formats a repository status into a colored string for terminal display.
///
/// Always returns a string. Callers decide whether to show it based on
/// `status.success`, `status.files_changed`, and `verbose`.
fn format_line(status: &RepoStatus) -> String {
    let name = status
        .path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| status.path.display().to_string());

    let color = if !status.success {
        COLOR_RED
    } else if status.files_changed > 0 {
        COLOR_GREEN
    } else {
        COLOR_SUBTEXT
    };

    format!("  {}: {}", name, status.message)
        .with(color)
        .to_string()
}

/// Removes duplicate paths from a Vec while preserving insertion order.
/// Uses a HashSet for O(1) duplicate detection.
fn unique_ordered(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new(); //  Track which paths we've encountered
    let mut out = Vec::with_capacity(dirs.len());    // Pre-allocate output vector
    for d in dirs {
        if seen.insert(d.clone()) {
            out.push(d); // Only add paths we haven't seen before
        }
    }
    out
}

/// Discovers Git repositories in the given directories.
///
///  For each directory:
///   - If the directory itself is a Git repo, add it and skip subdirectories
///   - Otherwise, scan immediate subdirectories for Git repos
///
/// Returns a deduplicated list of repository paths and a list of warning messages
/// for any directories that could not be read (e.g. permission denied, unmounted).
fn find_git_repositories(dirs: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let mut repos = Vec::new();
    let mut warnings = Vec::new();
    for d in dirs {
        let meta = match std::fs::metadata(d) {
            Ok(m) => m,
            Err(e) => {
                warnings.push(format!("cannot stat {}: {e}", d.display()));
                continue;
            }
        };
        if !meta.is_dir() {
            continue; // Skip non-directory paths
        }

        // Check if this directory is itself a Git repo
        if is_git_repo(d) {
            repos.push(d.clone());
            continue; // Don't recurse into subdirectories
        }

        // Scan immediate subdirectories for Git repos
        let entries = match std::fs::read_dir(d) {
            Ok(e) => e,
            Err(e) => {
                warnings.push(format!("cannot read {}: {e}", d.display()));
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && is_git_repo(&path) {
                repos.push(path); // Found a Git repo in subdirectory
            }
        }
    }
    (unique_ordered(repos), warnings)
}

/// Checks whether a path contains a valid Git repository.
/// Uses gitoxide's open function which validates the .git structure.
fn is_git_repo(path: &Path) -> bool {
    gix::open(path).is_ok()
}

///  Top-level wrapper for repository updates that converts errors into RepoStatus.
/// Ensures that any error from try_update_repository is caught and reported gracefully.
fn update_repository(path: &Path) -> RepoStatus {
    match try_update_repository(path) {
        Ok(status) => status,
        Err(e) => RepoStatus {
            path: path.to_path_buf(),
            success: false,
            message: e.to_string(),
            files_changed: 0,
        },
    }
}

/// Attempts to update a single Git repository via fetch + fast-forward.
///
/// The update process:
///   1. Open the repository with gitoxide
///   2. Check for local changes (bail if dirty)
///   3. Verify we're on a branch (bail if detached HEAD)
///   4. Fetch from the default remote using gitoxide's connect/prepare/receive pipeline
///   5. Find the updated remote tracking ref for our branch
///   6. Fast-forward the local branch ref to the new commit
///   7. Checkout the updated tree using `git checkout --force HEAD`
///   8. Count changed files by diffing the old and new tree
fn try_update_repository(path: &Path) -> Result<RepoStatus> {
    // Open the repository using gitoxide
    let repo = gix::open(path)?;

    // Bail early if the working tree has local modifications
    if repo.is_dirty()? {
        return Ok(RepoStatus {
            path: path.to_path_buf(),
            success: false,
            message: "Repository has local changes - skipping update".to_string(),
            files_changed: 0,
        });
    }

    // Get the current HEAD reference (must be a branch, not detached)
    let mut head_ref = match repo.head_ref()? {
        Some(r) => r,
        None => {
            return Ok(RepoStatus {
                path: path.to_path_buf(),
                success: false,
                message: "Detached HEAD state - skipping update".to_string(),
                files_changed: 0,
            });
        }
    };

    let old_id = head_ref.id().detach();
    let head_name = head_ref.name().as_bstr().to_string();

    // Resolve the default fetch remote (usually "origin")
    let remote = repo.find_default_remote(gix::remote::Direction::Fetch);
    let remote = match remote {
        Some(Ok(r)) => r,
        Some(Err(e)) => {
            return Ok(RepoStatus {
                path: path.to_path_buf(),
                success: false,
                message: format!("Remote error: {e}"),
                files_changed: 0,
            });
        }
        None => {
            return Ok(RepoStatus {
                path: path.to_path_buf(),
                success: false,
                message: "No remote configured".to_string(),
                files_changed: 0,
            });
        }
    };

    // Fetch from remote using gitoxide's three-step pipeline:
    // connect → prepare_fetch → receive
    let outcome = remote
        .connect(gix::remote::Direction::Fetch)?
        .prepare_fetch(gix::progress::Discard, Default::default())?
        .receive(gix::progress::Discard, &AtomicBool::new(false))?;

    //  Find the new commit ID from the fetch outcome's ref mappings
    let new_id = find_updated_target(&outcome, &head_name);

    let new_id = match new_id {
        Some(id) => id,
        None => {
            // No mapping found means nothing changed for our branch
            return Ok(RepoStatus {
                path: path.to_path_buf(),
                success: true,
                message: "Already up to date".to_string(),
                files_changed: 0,
            });
        }
    };

    // Compare old and new commit IDs
    if new_id == old_id {
        return Ok(RepoStatus {
            path: path.to_path_buf(),
            success: true,
            message: "Already up to date".to_string(),
            files_changed: 0,
        });
    }

    // Fast-forward: update the local branch ref to point at the new commit.
    // set_target_id uses PreviousValue::MustExistAndMatch internally, so it
    // fails atomically if the ref moved since we read it.
    head_ref.set_target_id(new_id, "groppy: fast-forward")?;

    // Walk the diff once: count changed files and apply deletions immediately.
    // Deletions are unlinked inline rather than collected into a Vec and iterated
    // again — gix::worktree::state::checkout only writes entries present in the new
    // index and does not remove files that disappeared.
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repo has no workdir"))?
        .to_owned();

    let old_tree = repo.find_object(old_id)?.peel_to_tree()?;
    let new_tree_obj = repo.find_object(new_id)?.peel_to_tree()?;
    let mut files_changed: usize = 0;

    old_tree
        .changes()?
        .options(|o| { o.track_path(); })
        .for_each_to_obtain_tree(&new_tree_obj, |change| {
            use gix::object::tree::diff::Change;
            match change {
                Change::Deletion { location, .. } => {
                    let _ = std::fs::remove_file(
                        workdir.join(gix::path::from_bstr(location).as_ref()),
                    );
                    files_changed += 1;
                }
                Change::Addition { .. } | Change::Modification { .. } | Change::Rewrite { .. } => {
                    files_changed += 1;
                }
            }
            Ok::<_, std::convert::Infallible>(gix::object::tree::diff::Action::Continue(()))
        })?;

    // Build the new index from the target tree and check out all modified/added entries.
    // Using overwrite_existing mirrors `git checkout --force HEAD`: existing files are
    // overwritten without complaint.
    let mut index = repo.index_from_tree(&new_tree_obj.id)?;
    let mut opts = repo.checkout_options(
        gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
    )?;
    opts.overwrite_existing = true;

    let checkout_result = gix::worktree::state::checkout(
        &mut index,
        &workdir,
        repo.objects.clone().into_arc()?,
        &gix::progress::Discard,
        &gix::progress::Discard,
        &AtomicBool::new(false),
        opts,
    );

    if let Err(e) = checkout_result {
        let msg = match head_ref.set_target_id(old_id, "groppy: revert failed fast-forward") {
            Ok(_) => format!("Checkout failed: {e}"),
            Err(revert_err) => format!("Checkout failed: {e}; revert also failed: {revert_err}"),
        };
        return Ok(RepoStatus {
            path: path.to_path_buf(),
            success: false,
            message: msg,
            files_changed: 0,
        });
    }

    // Persist the updated index so subsequent git commands and status checks see it
    index.write(Default::default())?;

    // Return success with the count of changed files
    Ok(RepoStatus {
        path: path.to_path_buf(),
        success: true,
        message: if files_changed > 0 {
            format!("Updated successfully - {files_changed} files changed")
        } else {
            "Updated successfully".to_string()
        },
        files_changed,
    })
}



/// Finds the updated commit ID for our branch in the fetch outcome.
///
/// Scans the ref mappings from the fetch to find one whose local tracking ref
/// (e.g., refs/remotes/origin/main) matches our branch name (e.g., refs/heads/main).
/// Returns the remote commit ID if found, or None if no update was fetched.
fn find_updated_target(
    outcome: &gix::remote::fetch::Outcome,
    head_name: &str,
) -> Option<gix::ObjectId> {
    //  Extract just the branch name (e.g., "main" from "refs/heads/main")
    let branch_short = head_name.strip_prefix("refs/heads/").unwrap_or(head_name);
    let tracking_suffix = format!("/{branch_short}");

    // Iterate through all ref mappings from the fetch
    for mapping in &outcome.ref_map.mappings {
        if let Some(ref local) = mapping.local {
            let local_bytes: &[u8] = local.as_ref();
            let local_str = local_bytes.to_str().unwrap_or("");
            // Match tracking refs that end with our branch name
            if local_str.ends_with(&tracking_suffix)
                && let Some(id) = mapping.remote.as_id()
            {
                return Some(id.to_owned());
            }
        }
    }
    None //  No matching ref found
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    // Helper: init a git repo with one commit via git CLI
    fn init_repo_with_commit(path: &Path) {
        fs::create_dir_all(path).unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(path)
            .output()
            .unwrap();
        fs::write(path.join("README.md"), "# test\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(path)
            .output()
            .unwrap();
    }

    // Helper: init a bare repo
    fn init_bare_repo(path: &Path) {
        fs::create_dir_all(path).unwrap();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(path)
            .output()
            .unwrap();
    }

    // ────────────────────────────────────────────────────────────
    // unique_ordered
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_unique_ordered_no_duplicates() {
        let input = vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")];
        let out = unique_ordered(input);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn test_unique_ordered_with_duplicates() {
        let input = vec![
            PathBuf::from("a"),
            PathBuf::from("b"),
            PathBuf::from("a"),
            PathBuf::from("c"),
            PathBuf::from("b"),
        ];
        let out = unique_ordered(input);
        assert_eq!(out, vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")]);
    }

    #[test]
    fn test_unique_ordered_all_same() {
        let input = vec![PathBuf::from("x"), PathBuf::from("x"), PathBuf::from("x")];
        let out = unique_ordered(input);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn test_unique_ordered_empty() {
        let out = unique_ordered(vec![]);
        assert!(out.is_empty());
    }

    // ────────────────────────────────────────────────────────────
    // is_git_repo
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_is_git_repo_valid() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());
        assert!(is_git_repo(tmp.path()));
    }

    #[test]
    fn test_is_git_repo_not_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_git_repo(tmp.path()));
    }

    #[test]
    fn test_is_git_repo_nonexistent() {
        assert!(!is_git_repo(Path::new("/nonexistent/path/to/repo")));
    }

    // ────────────────────────────────────────────────────────────
    // find_git_repositories
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_find_git_repos_direct_is_repo() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());
        let (repos, _) = find_git_repositories(&[tmp.path().to_path_buf()]);
        assert_eq!(repos.len(), 1);
    }

    #[test]
    fn test_find_git_repos_subdir_repos() {
        let parent = tempfile::tempdir().unwrap();
        let repo1 = parent.path().join("repo1");
        let repo2 = parent.path().join("repo2");
        let not_repo = parent.path().join("notrepo");
        fs::create_dir_all(&not_repo).unwrap();
        init_repo_with_commit(&repo1);
        init_repo_with_commit(&repo2);
        let (repos, _) = find_git_repositories(&[parent.path().to_path_buf()]);
        assert_eq!(repos.len(), 2);
    }

    #[test]
    fn test_find_git_repos_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let (repos, _) = find_git_repositories(&[tmp.path().to_path_buf()]);
        assert!(repos.is_empty());
    }

    #[test]
    fn test_find_git_repos_nonexistent_dir() {
        let (repos, _) = find_git_repositories(&[PathBuf::from("/nonexistent/path")]);
        assert!(repos.is_empty());
    }

    #[test]
    fn test_find_git_repos_file_path() {
        let tmp = tempfile::tempdir().unwrap();
        let fpath = tmp.path().join("file.txt");
        fs::write(&fpath, "hi").unwrap();
        let (repos, _) = find_git_repositories(&[fpath]);
        assert!(repos.is_empty());
    }

    #[test]
    fn test_find_git_repos_deduplication() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());
        let p = tmp.path().to_path_buf();
        let (repos, _) = find_git_repositories(&[p.clone(), p]);
        assert_eq!(repos.len(), 1);
    }

    #[test]
    fn test_find_git_repos_skips_subdir_if_parent_is_repo() {
        let parent = tempfile::tempdir().unwrap();
        init_repo_with_commit(parent.path());
        let nested = parent.path().join("nested");
        init_repo_with_commit(&nested);
        let (repos, _) = find_git_repositories(&[parent.path().to_path_buf()]);
        assert_eq!(repos.len(), 1);
    }

    // ────────────────────────────────────────────────────────────
    // format_line
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_format_line_success_with_changes() {
        let status = RepoStatus {
            path: PathBuf::from("/home/user/myrepo"),
            success: true,
            message: "Updated - 5 files changed".to_string(),
            files_changed: 5,
        };
        let line = format_line(&status);
        assert!(line.contains("myrepo"));
    }

    #[test]
    fn test_format_line_success_no_changes_not_verbose() {
        // format_line always returns a string; callers gate on files_changed/verbose
        let status = RepoStatus {
            path: PathBuf::from("/home/user/myrepo"),
            success: true,
            message: "Already up to date".to_string(),
            files_changed: 0,
        };
        let line = format_line(&status);
        assert!(line.contains("myrepo"));
    }

    #[test]
    fn test_format_line_success_no_changes_verbose() {
        let status = RepoStatus {
            path: PathBuf::from("/home/user/myrepo"),
            success: true,
            message: "Already up to date".to_string(),
            files_changed: 0,
        };
        let line = format_line(&status);
        assert!(line.contains("Already up to date"));
    }

    #[test]
    fn test_format_line_failure() {
        let status = RepoStatus {
            path: PathBuf::from("/home/user/myrepo"),
            success: false,
            message: "open repo: error".to_string(),
            files_changed: 0,
        };
        let line = format_line(&status);
        assert!(line.contains("open repo: error"));
    }

    #[test]
    fn test_format_line_uses_basename() {
        let status = RepoStatus {
            path: PathBuf::from("/a/very/deep/path/myrepo"),
            success: false,
            message: "error".to_string(),
            files_changed: 0,
        };
        let line = format_line(&status);
        assert!(line.contains("myrepo"));
    }

    #[test]
    fn test_format_line_root_path_fallback() {
        let status = RepoStatus {
            path: PathBuf::from("/"),
            success: false,
            message: "error".to_string(),
            files_changed: 0,
        };
        let line = format_line(&status);
        assert!(!line.is_empty());
    }

    // ────────────────────────────────────────────────────────────
    //  update_repository
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_update_repository_not_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let st = update_repository(tmp.path());
        assert!(!st.success);
    }

    #[test]
    fn test_update_repository_no_remote() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());
        let st = update_repository(tmp.path());
        assert!(!st.success);
        assert!(
            st.message.contains("No remote") || st.message.contains("remote"),
            "unexpected message: {}",
            st.message
        );
    }

    #[test]
    fn test_update_repository_dirty_repo() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());
        // Stage a new file
        fs::write(tmp.path().join("dirty.txt"), "dirty").unwrap();
        Command::new("git")
            .args(["add", "dirty.txt"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let st = update_repository(tmp.path());
        assert!(!st.success);
        assert!(st.message.contains("local changes"), "unexpected: {}", st.message);
    }

    #[test]
    fn test_update_repository_detached_head() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());
        // Detach HEAD
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Command::new("git")
            .args(["checkout", &hash])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let st = update_repository(tmp.path());
        assert!(!st.success);
        assert!(st.message.contains("Detached HEAD"), "unexpected: {}", st.message);
    }

    #[test]
    fn test_update_repository_already_up_to_date() {
        let tmp = tempfile::tempdir().unwrap();
        let bare_path = tmp.path().join("remote.git");
        let work_path = tmp.path().join("work");
        let clone_path = tmp.path().join("clone");

        // Create bare remote, push from work, clone
        init_bare_repo(&bare_path);
        init_repo_with_commit(&work_path);
        Command::new("git")
            .args(["remote", "add", "origin", bare_path.to_str().unwrap()])
            .current_dir(&work_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["push", "-u", "origin", "HEAD"])
            .current_dir(&work_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["clone", bare_path.to_str().unwrap(), clone_path.to_str().unwrap()])
            .output()
            .unwrap();

        let st = update_repository(&clone_path);
        assert!(st.success, "expected success, got: {}", st.message);
        assert_eq!(st.message, "Already up to date");
    }

    #[test]
    fn test_update_repository_with_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let bare_path = tmp.path().join("remote.git");
        let work_path = tmp.path().join("work");
        let clone_path = tmp.path().join("clone");

        init_bare_repo(&bare_path);
        init_repo_with_commit(&work_path);
        Command::new("git")
            .args(["remote", "add", "origin", bare_path.to_str().unwrap()])
            .current_dir(&work_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["push", "-u", "origin", "HEAD"])
            .current_dir(&work_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["clone", bare_path.to_str().unwrap(), clone_path.to_str().unwrap()])
            .output()
            .unwrap();

        // Add new commit and push from work
        fs::write(work_path.join("new.txt"), "new content").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&work_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add new file"])
            .current_dir(&work_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["push"])
            .current_dir(&work_path)
            .output()
            .unwrap();

        let st = update_repository(&clone_path);
        assert!(st.success, "expected success, got: {}", st.message);
        assert_eq!(st.files_changed, 1, "expected 1 file changed, got {}", st.files_changed);
    }

    // ────────────────────────────────────────────────────────────
    // is_dirty (via gix::Repository::is_dirty)
    // ────────────────────────────────────────────────────────────

    #[test]
    fn test_is_dirty_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().canonicalize().unwrap();
        init_repo_with_commit(&path);
        let repo = gix::open(&path).unwrap();
        assert!(!repo.is_dirty().unwrap());
    }

    #[test]
    fn test_is_dirty_staged_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().canonicalize().unwrap();
        init_repo_with_commit(&path);
        fs::write(path.join("staged.txt"), "staged").unwrap();
        Command::new("git")
            .args(["add", "staged.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        let repo = gix::open(&path).unwrap();
        assert!(repo.is_dirty().unwrap());
    }

    #[test]
    fn test_is_dirty_unstaged_modification() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().canonicalize().unwrap();
        init_repo_with_commit(&path);
        fs::write(path.join("README.md"), "modified content\n").unwrap();
        let repo = gix::open(&path).unwrap();
        assert!(repo.is_dirty().unwrap());
    }

    #[test]
    fn test_update_repository_unstaged_modification() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());
        fs::write(tmp.path().join("README.md"), "modified content\n").unwrap();

        let st = update_repository(tmp.path());
        assert!(!st.success);
        assert!(st.message.contains("local changes"), "unexpected: {}", st.message);
    }

    // ────────────────────────────────────────────────────────────
    // find_updated_target (needs a fetch outcome, tested via integration)
    // ────────────────────────────────────────────────────────────

    // find_updated_target is tested implicitly through update_repository_with_changes
    // Direct unit testing requires constructing gix::remote::fetch::Outcome which
    //    has non-public fields — covered via the integration tests above.

    // ────────────────────────────────────────────────────────────
    // run_spinner (trivial animation loop — no logic to test)
    // ────────────────────────────────────────────────────────────
}
