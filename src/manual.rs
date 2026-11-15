//! Local, page-preserving Force manual retrieval. No embeddings or cloud calls.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime},
};

pub const DOCUMENT_ID: &str = "force-v3.10.1";
const MAX_PDF: u64 = 64 * 1024 * 1024;
const MAX_TEXT: usize = 32 * 1024 * 1024;
const MAX_READ: usize = 64 * 1024;
const PASSAGE_CHARS: usize = 1800;

#[derive(Clone, Debug, Serialize)]
pub struct Metadata {
    pub document_id: String,
    pub title: String,
    pub model: String,
    pub manual_version: String,
    pub pdf_pages: usize,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Page {
    pub pdf_page: usize,
    pub printed_page: Option<String>,
    pub heading: String,
    pub citation: String,
    pub text: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Hit {
    pub pdf_page: usize,
    pub printed_page: Option<String>,
    pub heading: String,
    pub citation: String,
    pub excerpt: String,
    pub score: f64,
}
struct Passage {
    page: usize,
    text: String,
    terms: HashMap<String, usize>,
    len: usize,
}
pub struct Manual {
    pub metadata: Metadata,
    pages: Vec<Page>,
    passages: Vec<Passage>,
    frequencies: HashMap<String, usize>,
    average_length: f64,
    source: Option<(PathBuf, u64, Option<SystemTime>)>,
}
impl Manual {
    pub fn load_force(path: &Path) -> Result<Self, String> {
        let path = fs::canonicalize(path)
            .map_err(|e| format!("Cannot open manual {}: {e}", path.display()))?;
        let metadata = fs::metadata(&path).map_err(|e| format!("Cannot inspect manual: {e}"))?;
        if !metadata.is_file() || metadata.len() > MAX_PDF {
            return Err("Manual must be a PDF file of at most 64 MiB".into());
        }
        let mut hash = Sha256::new();
        let mut file = File::open(&path).map_err(|e| e.to_string())?;
        let mut buffer = [0u8; 64 * 1024];
        let mut read = 0u64;
        loop {
            let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            read += n as u64;
            if read > MAX_PDF {
                return Err("Manual grew beyond 64 MiB while importing".into());
            }
            hash.update(&buffer[..n]);
        }
        let mut command = Command::new("pdftotext");
        command
            .args(["-layout", "-enc", "UTF-8"])
            .arg(&path)
            .arg("-");
        let text = String::from_utf8(run_bounded(command, MAX_TEXT, Duration::from_secs(30))?)
            .map_err(|_| "pdftotext did not return UTF-8")?;
        let title_page = text.split('\u{c}').next().unwrap_or_default();
        let force_identified = title_page.contains("Force User Guide")
            || text.contains("Thank you for purchasing the Force.");
        if !title_page.contains("Manual Version v3.10.1") || !force_identified {
            return Err("Expected the Force User Guide with title-page Manual Version v3.10.1; refusing to mislabel a different manual".into());
        }
        let mut manual = Self::from_text(&text, format!("{:x}", hash.finalize()))?;
        manual.source = Some((path, metadata.len(), metadata.modified().ok()));
        Ok(manual)
    }
    fn from_text(text: &str, sha256: String) -> Result<Self, String> {
        if text.len() > MAX_TEXT {
            return Err("Extracted manual exceeds 32 MiB".into());
        }
        let mut texts: Vec<_> = text.split('\u{c}').collect();
        if texts.last().is_some_and(|s| s.trim().is_empty()) {
            texts.pop();
        }
        if texts.is_empty() || texts.len() > 2000 {
            return Err("Invalid manual page count".into());
        }
        let mut pages = Vec::new();
        let mut passages = Vec::new();
        let mut frequencies = HashMap::new();
        for (index, text) in texts.into_iter().enumerate() {
            let text = text.trim_matches(['\r', '\n']);
            let printed_page = text
                .lines()
                .rev()
                .map(str::trim)
                .find(|s| !s.is_empty())
                .filter(|s| s.len() <= 6 && s.chars().all(|c| c.is_ascii_digit()))
                .map(str::to_owned);
            let heading: String = text
                .lines()
                .map(str::trim)
                .find(|s| !s.is_empty())
                .unwrap_or("Untitled page")
                .chars()
                .take(160)
                .collect();
            let citation = format!(
                "Force User Guide v3.10.1, PDF p. {}{}",
                index + 1,
                printed_page
                    .as_ref()
                    .map(|p| format!(" (printed {p})"))
                    .unwrap_or_default()
            );
            pages.push(Page {
                pdf_page: index + 1,
                printed_page,
                heading,
                citation,
                text: text.into(),
            });
            let chars: Vec<_> = text.chars().collect();
            let mut start = 0;
            while start < chars.len() {
                let end = (start + PASSAGE_CHARS).min(chars.len());
                let text: String = chars[start..end].iter().collect();
                let tokens = tokens(&text);
                let len = tokens.len();
                let mut terms = HashMap::new();
                for term in tokens {
                    *terms.entry(term).or_insert(0usize) += 1;
                }
                for term in terms.keys() {
                    *frequencies.entry(term.clone()).or_insert(0usize) += 1;
                }
                passages.push(Passage {
                    page: index,
                    text,
                    terms,
                    len,
                });
                if end == chars.len() {
                    break;
                }
                start = end - 200;
            }
        }
        let average_length =
            passages.iter().map(|p| p.len).sum::<usize>() as f64 / passages.len().max(1) as f64;
        Ok(Self {
            metadata: Metadata {
                document_id: DOCUMENT_ID.into(),
                title: "Force User Guide".into(),
                model: "Akai Force".into(),
                manual_version: "v3.10.1".into(),
                pdf_pages: pages.len(),
                sha256,
            },
            pages,
            passages,
            frequencies,
            average_length: average_length.max(1.0),
            source: None,
        })
    }
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Hit>, String> {
        if query.len() > 512 || !(1..=8).contains(&limit) {
            return Err("Query must be at most 512 UTF-8 bytes; limit must be 1..=8".into());
        }
        let terms: HashSet<_> = tokens(query).into_iter().collect();
        if terms.is_empty() || terms.len() > 32 {
            return Err("Query needs 1..=32 searchable terms".into());
        }
        let phrase = query.trim().to_lowercase();
        let mut ranked: Vec<_> = self
            .passages
            .iter()
            .filter_map(|passage| {
                let mut score = 0.0;
                let mut matched = 0;
                for term in &terms {
                    let tf = *passage.terms.get(term).unwrap_or(&0) as f64;
                    if tf == 0.0 {
                        continue;
                    }
                    matched += 1;
                    let df = *self.frequencies.get(term).unwrap_or(&0) as f64;
                    let n = self.passages.len() as f64;
                    let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                    score += idf * tf * 2.2
                        / (tf + 1.2 * (0.25 + 0.75 * passage.len as f64 / self.average_length));
                }
                if matched == 0 {
                    return None;
                }
                score *= matched as f64 / terms.len() as f64;
                if passage.text.to_lowercase().contains(&phrase) {
                    score += 2.0;
                }
                Some((score, passage))
            })
            .collect();
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.page.cmp(&b.1.page)));
        let mut seen = HashSet::new();
        Ok(ranked
            .into_iter()
            .filter(|(_, p)| seen.insert(p.page))
            .take(limit)
            .map(|(score, p)| {
                let page = &self.pages[p.page];
                Hit {
                    pdf_page: page.pdf_page,
                    printed_page: page.printed_page.clone(),
                    heading: page.heading.clone(),
                    citation: page.citation.clone(),
                    excerpt: p.text.clone(),
                    score,
                }
            })
            .collect())
    }
    pub fn read(
        &self,
        document_id: &str,
        start_page: usize,
        page_count: usize,
    ) -> Result<Vec<Page>, String> {
        if document_id != self.metadata.document_id {
            return Err("Unknown document_id; use mpc_get_state or mpc_search_manual".into());
        }
        if start_page == 0 || !(1..=3).contains(&page_count) {
            return Err("Use 1-based PDF start_page and page_count in 1..=3".into());
        }
        let end = (start_page - 1)
            .checked_add(page_count)
            .ok_or("Page range overflow")?;
        let pages = self
            .pages
            .get(start_page - 1..end)
            .ok_or("Page range is outside the manual")?;
        if pages.iter().map(|p| p.text.len()).sum::<usize>() > MAX_READ {
            return Err("Requested pages exceed 64 KiB; read fewer pages".into());
        }
        Ok(pages.to_vec())
    }
    pub fn page_image(&self, pdf_page: usize) -> Result<Vec<u8>, String> {
        self.read(DOCUMENT_ID, pdf_page, 1)?;
        let (path, len, modified) = self
            .source
            .as_ref()
            .ok_or("No source PDF available for page rendering")?;
        let metadata = fs::metadata(path).map_err(|e| format!("Cannot inspect source PDF: {e}"))?;
        if metadata.len() != *len || metadata.modified().ok() != *modified {
            return Err(
                "Manual PDF changed since import; restart the MCP server before rendering".into(),
            );
        }
        let mut command = Command::new("pdftoppm");
        command
            .args([
                "-f",
                &pdf_page.to_string(),
                "-l",
                &pdf_page.to_string(),
                "-singlefile",
                "-scale-to",
                "1400",
                "-png",
            ])
            .arg(path);
        run_bounded(command, 8 * 1024 * 1024, Duration::from_secs(20))
    }
    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        Self::from_text("User Guide\nManual Version v3.10.1\u{c}Sample Edit\nUse Chop Mode to slice a drum loop.\n2\u{c}Mixer\nAdjust track volume and pan.\n3\u{c}", "synthetic".into()).unwrap()
    }
}
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .filter(|s| {
            !matches!(
                s.as_str(),
                "the"
                    | "a"
                    | "an"
                    | "to"
                    | "of"
                    | "and"
                    | "or"
                    | "is"
                    | "in"
                    | "on"
                    | "for"
                    | "with"
                    | "how"
                    | "do"
                    | "i"
                    | "it"
                    | "this"
                    | "that"
            )
        })
        .collect()
}

/// Drain bounded stdout on one reader thread while enforcing a child deadline.
/// Poppler is launched directly, never through a shell. stderr cannot fill a pipe.
fn run_bounded(mut command: Command, limit: usize, timeout: Duration) -> Result<Vec<u8>, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Cannot run {program}: {e}. Install Poppler (pdftotext/pdftoppm)."))?;
    let stdout = child.stdout.take().ok_or("Missing subprocess stdout")?;
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = tx.send(result);
    });
    let deadline = Instant::now() + timeout;
    let mut output = None;
    let result = loop {
        if output.is_none()
            && let Ok(bytes) = rx.try_recv()
        {
            output = Some(bytes);
        }
        if output
            .as_ref()
            .is_some_and(|r| r.as_ref().is_ok_and(|b| b.len() > limit))
        {
            break Err(format!("{program} output exceeded its size limit"));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err(format!("{program} failed to process the manual"))
                };
            }
            Err(e) => break Err(format!("Cannot wait for {program}: {e}")),
            Ok(None) => (),
        }
        if Instant::now() >= deadline {
            break Err(format!("{program} exceeded its time limit"));
        }
        thread::sleep(Duration::from_millis(10));
    };
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let _ = reader.join();
    result?;
    let bytes = output
        .unwrap_or_else(|| {
            rx.recv()
                .unwrap_or_else(|_| Err(std::io::Error::other("Subprocess reader stopped")))
        })
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err(format!("{program} output exceeded its size limit"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn page_boundaries_citations_and_ranked_search() {
        let manual = Manual::fixture();
        assert_eq!(manual.metadata.pdf_pages, 3);
        let hits = manual.search("How do I chop a drum loop?", 5).unwrap();
        assert_eq!(hits[0].pdf_page, 2);
        assert_eq!(hits[0].printed_page.as_deref(), Some("2"));
        assert!(hits[0].citation.contains("v3.10.1, PDF p. 2"));
        assert!(!hits[0].excerpt.contains("Mixer"));
        assert_eq!(manual.read(DOCUMENT_ID, 3, 1).unwrap()[0].heading, "Mixer");
        assert!(manual.search("unfindableword", 2).unwrap().is_empty());
    }
    #[test]
    fn invalid_queries_documents_and_ranges_are_bounded() {
        let manual = Manual::fixture();
        for query in ["", "the and to", &"x".repeat(513)] {
            assert!(manual.search(query, 1).is_err());
        }
        assert!(manual.search("chop", 9).is_err());
        for (page, count) in [(0, 1), (1, 0), (1, 4), (3, 2), (usize::MAX, 3)] {
            assert!(manual.read(DOCUMENT_ID, page, count).is_err());
        }
        assert!(manual.read("wrong-document", 1, 1).is_err());
        assert!(manual.page_image(1).is_err());
    }
    #[test]
    fn subprocess_output_and_time_limits() {
        let mut command = Command::new("printf");
        command.arg("12345");
        assert!(
            run_bounded(command, 4, Duration::from_secs(2))
                .unwrap_err()
                .contains("size limit")
        );
        let mut command = Command::new("sleep");
        command.arg("2");
        let start = Instant::now();
        assert!(
            run_bounded(command, 16, Duration::from_millis(30))
                .unwrap_err()
                .contains("time limit")
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
