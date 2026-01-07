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
    pub canonical_path: String,
    pub page: usize,
    pub excerpt: String,
}

pub async fn fuzzy_search_pdfs(conn: &libsql::Connection, query: &str) -> Result<Vec<PdfMatch>> {
    let mut rows = conn
        .query("SELECT canonical_base_path FROM papers", ())
        .await?;

    let mut all_matches = Vec::new();
    let mut matcher = Nucleo::new(Config::DEFAULT, Arc::new(|| {}), None, 1);
    let injector = matcher.injector();
    matcher.pattern.reparse(
        0,
        query,
        nucleo::pattern::CaseMatching::Ignore,
        nucleo::pattern::Normalization::Smart,
        false,
    );

    while let Some(row) = rows.next().await? {
        let base_path_str: String = row.get(0)?;
        let base_path = Path::new(&base_path_str);
        let pdf_path = base_path.join("paper.pdf");

        if !pdf_path.exists() {
            continue;
        }

        for (i, page_text) in pdf_extract::extract_text_by_pages(pdf_path)?
            .into_iter()
            .enumerate()
        {
            if page_text.trim().is_empty() {
                continue;
            }

            injector.push(
                (page_text, i, base_path_str.clone()),
                |haystack, columns| {
                    columns[0] = Utf32String::from(haystack.0.as_str());
                },
            );
        }
    }

    matcher.tick(100000);

    let snapshot = matcher.snapshot();
    for matched_item in snapshot.matched_items(0..snapshot.matched_item_count()) {
        // Create a small excerpt (first 100 chars of the paragraph for context)
        let excerpt = matched_item.data.0.chars().take(120).collect::<String>();

        all_matches.push(PdfMatch {
            canonical_path: matched_item.data.2.clone(),
            page: matched_item.data.1 + 1, // 1-indexed for humans
            excerpt: format!("{}...", excerpt.trim().replace('\n', " (new line) ")),
        });
    }

    Ok(all_matches)
}
