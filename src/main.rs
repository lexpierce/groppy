use anyhow::{Context, Result};
use clap::Parser;
use crossbeam::channel;
use git2::{Repository, StatusOptions};
use indicatif::{ProgressBar, ProgressStyle};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

// Catppuccin Macchiato colors
const RED: &str = "\x1b[38;2;237;135;150m";
const GREEN: &str = "\x1b[38;2;166;218;149m";
const OVERLAY0: &str = "\x1b[38;2;110;115;141m";
const RESET: &str = "\x1b[0m";

#[derive(Parser, Debug)]
#[command(name = "groppy")]
#[command(about = "Update multiple git repositories in parallel")]
struct Args {
    /// Number of threads to use
    #[arg(short = 'j', default_value = "4")]
    threads: usize,

    /// Directories to check for git repositories
    directories: Vec<PathBuf>,
}

struct Stats {
    checked: AtomicUsize,
    updated: AtomicUsize,
    unclean: AtomicUsize,
    total: usize,
}

impl Stats {
    fn new(total: usize) -> Self {
        Stats {
            checked: AtomicUsize::new(0),
            updated: AtomicUsize::new(0),
            unclean: AtomicUsize::new(0),
            total,
        }
    }

    fn inc_updated(&self) {
        self.updated.fetch_add(1, Ordering::SeqCst);
    }

    fn inc_unclean(&self) {
        self.unclean.fetch_add(1, Ordering::SeqCst);
    }
}

fn update_progress(current: usize, total: usize) {
    let percent = (current as f64 / total as f64 * 100.0) as u32;
    print!("\x1b]9;4;1;{}\x07", percent);
    io::stdout().flush().ok();
}

fn is_git_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

fn find_repos(directories: &[PathBuf]) -> Vec<PathBuf> {
    let mut repos = Vec::new();

    for dir in directories {
        if !dir.exists() {
            continue;
        }

        // Check if the directory itself is a git repo
        if is_git_repo(dir) {
            repos.push(dir.clone());
        }

        // Look one level down
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && is_git_repo(&path) {
                    repos.push(path);
                }
            }
        }
    }

    repos
}

fn is_repo_clean(repo: &Repository) -> Result<bool> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true);
    opts.include_ignored(false);

    let statuses = repo.statuses(Some(&mut opts))?;
    Ok(statuses.is_empty())
}

fn update_repo(repo_path: &Path, stats: &Stats, pb: &ProgressBar) -> Result<()> {
    let checked = stats.checked.fetch_add(1, Ordering::SeqCst) + 1;
    update_progress(checked, stats.total);
    pb.set_message(format!("Updating repositories… ({}/{})", checked, stats.total));

    let repo = Repository::open(repo_path)
        .with_context(|| format!("Failed to open repository: {}", repo_path.display()))?;

    // Check if repo is clean
    let is_clean = is_repo_clean(&repo)?;
    if !is_clean {
        stats.inc_unclean();
        pb.println(format!(
            "{}Repository not clean: {}{}",
            RED,
            repo_path.display(),
            RESET
        ));
        return Ok(());
    }

    // Get current HEAD
    let head = repo.head()?;
    let head_commit = head.peel_to_commit()?;
    let old_oid = head_commit.id();

    // Fetch with callbacks for SSH/HTTPS authentication
    let mut remote = repo.find_remote("origin")?;
    let mut fetch_options = git2::FetchOptions::new();
    let mut callbacks = git2::RemoteCallbacks::new();
    
    // SSH key authentication
    callbacks.credentials(|_url, username_from_url, _allowed_types| {
        git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
    });
    
    fetch_options.remote_callbacks(callbacks);
    remote.fetch(&["HEAD"], Some(&mut fetch_options), None)?;

    // Get the upstream branch
    let branch = repo.head()?;
    let branch_name = branch
        .shorthand()
        .ok_or_else(|| anyhow::anyhow!("Could not get branch name"))?;

    let upstream_name = repo
        .branch_upstream_name(&format!("refs/heads/{}", branch_name))
        .ok();

    if upstream_name.is_none() {
        return Ok(());
    }

    let upstream_name_str = upstream_name.unwrap();
    let upstream_name_str = upstream_name_str
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Could not convert upstream name"))?;

    let upstream_ref = repo.find_reference(upstream_name_str)?;
    let upstream_commit = upstream_ref.peel_to_commit()?;
    let upstream_oid = upstream_commit.id();

    // Check if update is needed
    if old_oid == upstream_oid {
        return Ok(());
    }

    // Perform fast-forward merge
    let mut reference = repo.head()?;
    reference.set_target(upstream_oid, "fast-forward merge")?;
    repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))?;

    // Count changed files
    let new_commit = repo.find_commit(upstream_oid)?;
    let old_tree = head_commit.tree()?;
    let new_tree = new_commit.tree()?;
    let diff = repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), None)?;
    let files_changed = diff.deltas().len();

    stats.inc_updated();
    pb.println(format!(
        "{}Updated: {} ({} files changed){}",
        GREEN,
        repo_path.display(),
        files_changed,
        RESET
    ));

    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.threads == 0 {
        anyhow::bail!("Number of threads must be at least 1");
    }

    let repos = find_repos(&args.directories);

    if repos.is_empty() {
        println!("No repositories found");
        return Ok(());
    }

    let total_repos = repos.len();
    let stats = Arc::new(Stats::new(total_repos));
    let (sender, receiver) = channel::bounded(total_repos);

    // Create spinner
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")?
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.set_message(format!("Updating repositories… (0/{})", total_repos));
    pb.enable_steady_tick(std::time::Duration::from_millis(100));
    let pb = Arc::new(pb);

    // Send initial OSC 9;4 progress
    update_progress(0, total_repos);

    // Send all repos to the channel
    for repo in repos {
        sender.send(repo).ok();
    }
    drop(sender);

    // Spawn worker threads
    let mut handles = Vec::new();
    for _ in 0..args.threads {
        let receiver = receiver.clone();
        let stats = Arc::clone(&stats);
        let pb = Arc::clone(&pb);

        let handle = thread::spawn(move || {
            while let Ok(repo_path) = receiver.recv() {
                if let Err(e) = update_repo(&repo_path, &stats, &pb) {
                    pb.println(format!("Error updating {}: {}", repo_path.display(), e));
                }
            }
        });

        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().ok();
    }

    pb.finish_and_clear();

    // Send final OSC 9;4 progress (complete)
    print!("\x1b]9;4;0\x07");
    io::stdout().flush().ok();

    // Print summary
    println!(
        "{}Checked: {}, Updated: {}, Unclean: {}{}",
        OVERLAY0,
        stats.checked.load(Ordering::SeqCst),
        stats.updated.load(Ordering::SeqCst),
        stats.unclean.load(Ordering::SeqCst),
        RESET
    );

    Ok(())
}
