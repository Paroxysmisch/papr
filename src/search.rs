use anyhow::Result;
use nucleo::Nucleo;
use nucleo_matcher::{
    Config, Matcher, Utf32String,
    pattern::{Atom, AtomKind, CaseMatching, Normalization},
};
use std::fmt;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug)]
pub struct PaperMatch {
    pub id: u32,
    pub canonical_base_path: String,
    url: String,
    score: u32,
}

impl fmt::Display for PaperMatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Path: {}\nURL: {}\nID: {}\nScore: {}",
            self.canonical_base_path, self.url, self.id, self.score
        )
    }
}

pub async fn fuzzy_search_papers(
    conn: &libsql::Connection,
    query: &str,
) -> Result<Vec<PaperMatch>> {
    let mut rows = conn
        .query("SELECT id, canonical_base_path, url FROM papers", ())
        .await?;

    let needle = Atom::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
        false,
    );
    let mut matcher = Matcher::new(Config::DEFAULT);

    let mut res = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: u32 = row.get(0)?;
        let canonical_base_path: String = row.get(1)?;
        let url: String = row.get(2)?;

        // Extract the folder name (the paper title) from the path
        let title = Path::new(&canonical_base_path)
            .file_name()
            .and_then(|os_str| os_str.to_str())
            .unwrap_or("")
            .to_string();
        let list_title = [&title];

        if !title.is_empty() {
            let matches = needle.match_list(list_title, &mut matcher);

            if let Some((_, score)) = matches.into_iter().next() {
                res.push(PaperMatch {
                    id,
                    canonical_base_path,
                    url,
                    score: (score as u32),
                });
            }
        }
    }

    Ok(res)
}

#[derive(Debug)]
pub struct PdfMatch {
    pub title: String,
    pub page: usize,
    pub score: u32,
    pub excerpt: String,
}

pub async fn fuzzy_search_pdfs(conn: &libsql::Connection, query: &str) -> Result<Vec<PdfMatch>> {
    let mut rows = conn
        .query("SELECT canonical_base_path FROM papers", ())
        .await?;

    let mut all_matches = Vec::new();
    while let Some(row) = rows.next().await? {
        let base_path_str: String = row.get(0)?;
        let base_path = Path::new(&base_path_str);
        let pdf_path = base_path.join("paper.pdf");

        let title = base_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string();

        if !pdf_path.exists() {
            continue;
        }

        // Load the PDF
        let bytes = std::fs::read(&pdf_path)?;

        // pdf-extract can provide page-by-page output
        let out = pdf_extract::extract_text_from_mem(&bytes)?;

        // Split by Form Feed character (common page separator in extraction)
        // Note: some PDFs require more complex page-splitting depending on the library

        for (i, page_text) in out.split('\u{000c}').enumerate() {
            if page_text.trim().is_empty() {
                continue;
            }

            // Setup a local matcher for this page to get a score
            let mut matcher = Nucleo::new(Config::DEFAULT, Arc::new(|| {}), None, 1);
            let injector = matcher.injector();
            injector.push(String::from(page_text), |haystack, columns| {
                columns[0] = Utf32String::from(haystack.as_str());
            });

            // Pattern match
            matcher.pattern.reparse(
                0,
                query,
                nucleo::pattern::CaseMatching::Ignore,
                nucleo::pattern::Normalization::Smart,
                false,
            );
            matcher.tick(100000);

            let snapshot = matcher.snapshot();
            if let Some(matched_item) = snapshot
                .matched_items(0..snapshot.matched_item_count())
                .next()
            {
                // Create a small excerpt (first 100 chars of the page for context)
                let excerpt = matched_item
                    .data
                    .chars()
                    .take(120)
                    .collect::<String>()
                    .replace('\n', " ");

                all_matches.push(PdfMatch {
                    title: title.clone(),
                    page: i + 1, // 1-indexed for humans
                    score: 0,
                    excerpt: format!("{}...", excerpt.trim()),
                });
            }
        }
    }

    // Sort by score descending
    all_matches.sort_by(|a, b| b.score.cmp(&a.score));
    Ok(all_matches)
}
