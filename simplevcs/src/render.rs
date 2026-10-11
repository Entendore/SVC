use crate::error::{VcsError, VcsResult};
use crate::repo::Repo;
use ab_glyph::{Font, FontVec, Scale};
use image::RgbImage;
use imageproc::drawing::draw_text_mut;
use std::io::Write;
use std::process::{Command, Stdio, ChildStdin};

fn check_ffmpeg() -> VcsResult<()> {
    let status = Command::new("ffmpeg").arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => Err(VcsError::FfmpegNotFound),
    }
}

fn load_font() -> FontVec {
    let paths = ["/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", "font.ttf"];
    for p in paths {
        if let Ok(data) = std::fs::read(p) {
            if let Ok(font) = FontVec::try_from_vec(data) { return font; }
        }
    }
    panic!("No suitable monospace font found.");
}

fn get_author_color_rgb(author_id: usize) -> image::Rgb<u8> {
    let colors = [
        image::Rgb([235, 99, 99]), image::Rgb([99, 235, 99]), image::Rgb([99, 132, 235]),
        image::Rgb([235, 219, 99]), image::Rgb([204, 99, 235]), image::Rgb([99, 235, 224]),
    ];
    colors[author_id % colors.len()]
}

impl Repo {
    pub fn render(&self, filename: &str, preset: &str) -> VcsResult<()> {
        check_ffmpeg()?;
        let head_hash = self.read_head()?;
        if head_hash.is_empty() { return Ok(()); }

        let ancestors = self.get_ancestors(&head_hash)?;
        let mut sorted: Vec<crate::odb::Commit> = ancestors.iter().filter_map(|h| self.read_commit(h).ok()).collect();
        sorted.sort_by(|a, b| a.ts_ms.cmp(&b.ts_ms));

        let font = load_font();
        let mut authors_map: std::collections::HashMap<std::rc::Rc<str>, usize> = std::collections::HashMap::new();
        let mut current_text: Vec<(char, usize)> = Vec::new();
        let mut canvas = RgbImage::from_pixel(1280, 720, image::Rgb([24, 24, 27]));
        let mp4_name = format!("{}.mp4", filename);

        let mut child = Command::new("ffmpeg")
            .args(["-y", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", "1280x720", "-framerate", "60", "-i", "pipe:0", "-c:v", "libx264", "-preset", preset, "-tune", "zerolatency", "-crf", "18", "-pix_fmt", "yuv420p", "-threads", "0", &mp4_name])
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::inherit()).spawn()?;

        let mut stdin = child.stdin.take().ok_or(VcsError::FfmpegFailed)?;

        for commit in &sorted {
            let author_rc: std::rc::Rc<str> = std::rc::Rc::from(commit.author.as_str());
            let author_id = *authors_map.entry(author_rc).or_insert_with(|| authors_map.len());
            let tree = self.read_tree(&commit.tree)?;
            let new_bytes = tree.files.get(filename).map(|e| self.read_blob(&e.hash).unwrap_or_default()).unwrap_or_default();
            if crate::odb::is_binary(&new_bytes) { continue; }

            let new_content = String::from_utf8_lossy(&new_bytes).to_string();
            let new_chars: Vec<char> = new_content.chars().collect();
            let old_chars: Vec<char> = current_text.iter().map(|(c, _)| *c).collect();
            let diff_ops = crate::odb::diff_lcs(&old_chars, &new_chars);
            
            let mut render_state = current_text.clone();
            let mut newly_added_indices = std::collections::HashSet::new();

            for (c, old_idx, _) in diff_ops {
                if old_idx.is_none() {
                    render_state.push((c, author_id));
                    newly_added_indices.insert(render_state.len() - 1);
                    if c != '\n' {
                        draw_and_send_scrolled(&render_state, &newly_added_indices, &mut canvas, &font, &mut stdin, commit)?;
                    }
                }
            }
            current_text = render_state;
            newly_added_indices.clear();
            for _ in 0..90 {
                draw_and_send_scrolled(&current_text, &newly_added_indices, &mut canvas, &font, &mut stdin, commit)?;
            }
        }
        drop(stdin);
        let status = child.wait()?;
        if !status.success() { return Err(VcsError::FfmpegFailed); }
        Ok(())
    }
}

fn draw_and_send_scrolled(
    state: &[(char, usize)],
    new_indices: &std::collections::HashSet<usize>,
    canvas: &mut RgbImage,
    font: &FontVec,
    stdin: &mut ChildStdin,
    commit: &crate::odb::Commit
) -> VcsResult<()> {
    canvas.as_raw_mut().fill(24);
    let scale = Scale::uniform(24.0);
    let header_color = image::Rgb([200, 200, 200]);
    let short_hash = commit.hash.get(..7).unwrap_or(&commit.hash);
    draw_text_mut(canvas, header_color, 40, 20, scale, font, &format!("Commit: {} | Author: {}", short_hash, commit.author));
    for x in 0..1280 { canvas.put_pixel(x, 50, image::Rgb([60, 60, 60])); }

    let line_height = 32.0;
    let max_visible_lines = ((720.0 - 60.0) / line_height) as usize;
    let total_lines = state.iter().filter(|(c, _)| *c == '\n').count() + 1;
    let scroll_offset = if total_lines > max_visible_lines { total_lines - max_visible_lines } else { 0 };

    let text_scale = Scale::uniform(26.0);
    let mut x = 40.0;
    let mut y = 60.0 - (scroll_offset as f32 * line_height);
    let mut current_line = 0;
    let mut buf = [0u8; 4];

    for (i, (ch, author_id)) in state.iter().enumerate() {
        if *ch == '\n' {
            current_line += 1;
            y += line_height;
            x = 40.0;
            if current_line >= scroll_offset + max_visible_lines { break; }
            continue;
        }
        if current_line >= scroll_offset {
            let base_color = get_author_color_rgb(*author_id);
            let color = if new_indices.contains(&i) {
                image::Rgb([base_color.0[0], base_color.0[1], base_color.0[2]])
            } else {
                image::Rgb([base_color.0[0] / 3, base_color.0[1] / 3, base_color.0[2] / 3])
            };
            let s = ch.encode_utf8(&mut buf);
            draw_text_mut(canvas, color, x as i32, y as i32, text_scale, font, s);
            let advance = if let Some(glyph_id) = font.glyph_id(*ch) { glyph_id.with_scale(text_scale).h_advance() } else { 16.0 };
            if x + advance > 1240.0 { y += line_height; x = 40.0; } else { x += advance; }
        }
    }
    stdin.write_all(canvas.as_raw())?;
    Ok(())
}
