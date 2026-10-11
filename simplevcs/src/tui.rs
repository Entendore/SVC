use crate::error::{VcsError, VcsResult};
use crate::repo::Repo;
use crate::odb::diff_lcs;
use std::io::{self, Write};

fn get_author_color(author: &str) -> &'static str {
    let colors = ["\x1B[31m", "\x1B[32m", "\x1B[33m", "\x1B[34m", "\x1B[35m", "\x1B[36m"];
    let mut hash = 0u32;
    for c in author.chars() { hash = hash.wrapping_add(c as u32); hash = hash.wrapping_mul(31); }
    colors[(hash as usize) % colors.len()]
}

impl Repo {
    pub fn typewriter(&self, filename: &str) -> VcsResult<()> {
        let head_hash = self.read_head()?;
        if head_hash.is_empty() { return Ok(()); }

        let ancestors = self.get_ancestors(&head_hash)?;
        let mut sorted: Vec<crate::odb::Commit> = ancestors.iter().filter_map(|h| self.read_commit(h).ok()).collect();
        sorted.sort_by(|a, b| a.ts_ms.cmp(&b.ts_ms));

        let mut current_state: Vec<(char, String)> = Vec::new();
        let stdout = io::stdout();
        let mut handle = stdout.lock();

        for commit in &sorted {
            let tree = self.read_tree(&commit.tree)?;
            let new_bytes = tree.files.get(filename).map(|e| self.read_blob(&e.hash).unwrap_or_default()).unwrap_or_default();
            if crate::odb::is_binary(&new_bytes) { continue; }

            let new_content = String::from_utf8_lossy(&new_bytes).to_string();
            let new_chars: Vec<char> = new_content.chars().collect();
            let old_chars: Vec<char> = current_state.iter().map(|(c, _)| *c).collect();
            let diff_ops = diff_lcs(&old_chars, &new_chars);

            let mut new_state = Vec::with_capacity(new_chars.len());
            write!(handle, "\x1B[H\x1B[2J")?;
            let short_hash = commit.hash.get(..7).unwrap_or(&commit.hash);
            writeln!(handle, "\x1B[1;36mCommit: {}\x1B[0m | \x1B[1;33mAuthor: {}\x1B[0m", short_hash, commit.author)?;
            writeln!(handle, "\x1B[90m{}\x1B[0m", "─".repeat(50))?;

            for (c, old_idx, _) in diff_ops {
                let author = if let Some(idx) = old_idx { current_state[idx].1.clone() } else { commit.author.clone() };
                let is_new = old_idx.is_none();
                new_state.push((c, author.clone()));
                let color = get_author_color(&author);
                if is_new {
                    write!(handle, "{}{}\x1B[0m", color, c)?;
                    if c != '\n' {
                        handle.flush()?;
                        std::thread::sleep(std::time::Duration::from_millis(15));
                    }
                } else {
                    write!(handle, "\x1B[90m{}\x1B[0m", c)?;
                }
            }
            current_state = new_state;
            handle.flush()?;
            std::thread::sleep(std::time::Duration::from_millis(800));
        }
        Ok(())
    }
}
