use anyhow::Result;
use nucleo_matcher::{
    Config, Matcher,
    pattern::{Atom, AtomKind, CaseMatching, Normalization},
};
use std::fmt;
use std::path::Path;

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

        if title.len() > 0 {
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
