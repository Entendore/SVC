use clap::{Parser, Subcommand};
use crate::commands;

#[derive(Parser, Debug)]
#[command(name = "simplevcs")]
#[command(about = "A high-performance, strictly linear VCS", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    Init { #[arg(default_value = "default-repo")] name: String },
    Add { paths: Vec<String> },
    Commit {
        message: String,
        #[arg(short, long, default_value = "Unknown")]
        author: String,
    },
    Merge {
        target_hashes: Vec<String>,
        #[arg(short, long, default_value = "Merge Bot")]
        author: String,
        #[arg(short, long)]
        message: Option<String>,
    },
    Delete { target_hash: String },
    Restore { target_hash: String },
    Blame { file: String },
    Stats { #[arg(short, long)] author: Option<String> },
    Graph,
    Log {
        #[arg(short, long)]
        author: Option<String>,
        #[arg(short = 's', long)]
        stats: bool,
    },
    Gc,
    Status,
    Diff {
        commit1: Option<String>,
        commit2: Option<String>,
    },
}

impl Cli {
    pub fn execute(&self) -> Result<(), crate::VcsError> {
        match &self.command {
            Commands::Init { name } => commands::init(name),
            Commands::Add { paths } => commands::add(paths, false, false, false, false),
            Commands::Commit { message, author } => commands::commit(message, author, Vec::new()),
            Commands::Merge { target_hashes, author, message } => 
                commands::merge(target_hashes, author.clone(), message.clone(), false, false, false, false, false, "manual".to_string(), false, false, None),
            Commands::Delete { target_hash } => commands::delete(target_hash, false, false, false),
            Commands::Restore { target_hash } => commands::restore(target_hash, &[], false, false),
            Commands::Blame { file } => commands::blame(file, None, false),
            Commands::Stats { author } => commands::stats(author.clone()),
            Commands::Graph => commands::graph(),
            Commands::Log { author, stats } => commands::log(author.clone(), *stats),
            Commands::Gc => commands::gc(),
            Commands::Status => commands::status(),
            Commands::Diff { commit1, commit2 } => commands::diff(commit1.clone(), commit2.clone()),
        }
    }
}
