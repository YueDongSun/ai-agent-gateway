//! Statistics-backed collection from the official LMArena Dataset Viewer.
//! Complete category statistics provide an independent overall-row target;
//! unfiltered rows are scanned without assuming any category ordering.
//! Totals and publication dates are checked on a best-effort remote snapshot.
//! The API supplies no atomic revision binding between statistics and pages.

use super::arena;
use super::parse::{ParseError, MAX_DOCUMENT_BYTES};
use super::sources::{self, FetchFailure};
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

const VIEWER_ORIGIN: &str = "https://datasets-server.huggingface.co";
const DATASET_ID: &str = "lmarena-ai/leaderboard-dataset";
const PAGE_LENGTH: usize = 100;
const MAX_PAGES: usize = 200;
const MAX_DATASET_ROWS: usize = MAX_PAGES * PAGE_LENGTH;
const FETCH_DEADLINE: Duration = Duration::from_secs(90);

pub(crate) async fn fetch_arena_document() -> Result<String, FetchFailure> {
    let started = Instant::now();
    tokio::time::timeout(FETCH_DEADLINE, async {
        let statistics_url = viewer_url("/statistics")?;
        let statistics =
            sources::fetch_document(statistics_url.as_str(), MAX_DOCUMENT_BYTES).await?;
        let mut collection = RowCollection::from_statistics(&statistics)?;
        loop {
            if started.elapsed() >= FETCH_DEADLINE {
                return Err(FetchFailure::Timeout);
            }
            let offset = collection.next_offset()?;
            let url = rows_url(offset)?;
            let page_started = Instant::now();
            let fetched = sources::fetch_document(url.as_str(), MAX_DOCUMENT_BYTES).await;
            #[cfg(test)]
            println!(
                "arena_public_page offset={} elapsed_ms={} outcome={}",
                offset,
                page_started.elapsed().as_millis(),
                fetched
                    .as_ref()
                    .map(|_| "received")
                    .unwrap_or_else(|failure| failure.code())
            );
            #[cfg(not(test))]
            let _ = page_started;
            if collection.append(&fetched?, offset)? {
                let document = collection.finish(chrono::Utc::now().date_naive())?;
                return if started.elapsed() < FETCH_DEADLINE {
                    Ok(document)
                } else {
                    Err(FetchFailure::Timeout)
                };
            }
        }
    })
    .await
    .map_err(|_| FetchFailure::Timeout)?
}

fn viewer_url(path: &str) -> Result<reqwest::Url, FetchFailure> {
    if !matches!(path, "/statistics" | "/rows") {
        return Err(FetchFailure::Decode);
    }
    let mut url = reqwest::Url::parse(VIEWER_ORIGIN).map_err(|_| FetchFailure::Transport)?;
    url.set_path(path);
    url.query_pairs_mut()
        .append_pair("dataset", DATASET_ID)
        .append_pair("config", arena::SOURCE_CONFIG)
        .append_pair("split", arena::SOURCE_SPLIT);
    Ok(url)
}

fn rows_url(offset: usize) -> Result<reqwest::Url, FetchFailure> {
    if offset >= MAX_DATASET_ROWS || offset % PAGE_LENGTH != 0 {
        return Err(FetchFailure::Decode);
    }
    let mut url = viewer_url("/rows")?;
    url.query_pairs_mut()
        .append_pair("offset", &offset.to_string())
        .append_pair("length", &PAGE_LENGTH.to_string());
    Ok(url)
}

fn decode_document(raw: &str) -> Result<Value, FetchFailure> {
    let root: Value = serde_json::from_str(raw).map_err(|_| FetchFailure::Decode)?;
    if !root.is_object()
        || root.get("error").is_some_and(|value| !value.is_null())
        || root.get("error_code").is_some_and(|value| !value.is_null())
        || root
            .get("partial")
            .is_some_and(|value| value.as_bool() != Some(false))
    {
        return Err(FetchFailure::Decode);
    }
    Ok(root)
}

fn bounded_count(value: &Value) -> Result<usize, FetchFailure> {
    let count = if let Some(value) = value.as_u64() {
        value
    } else {
        value
            .as_f64()
            .filter(|value| {
                value.is_finite()
                    && *value >= 0.0
                    && value.fract() == 0.0
                    && *value <= MAX_DATASET_ROWS as f64
            })
            .ok_or(FetchFailure::Decode)? as u64
    };
    if count > MAX_DATASET_ROWS as u64 {
        return Err(FetchFailure::TooLarge);
    }
    Ok(count as usize)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DatasetStatistics {
    dataset_rows: usize,
    overall_rows: usize,
}

fn parse_statistics(raw: &str) -> Result<DatasetStatistics, FetchFailure> {
    if raw.len() > MAX_DOCUMENT_BYTES {
        return Err(FetchFailure::TooLarge);
    }
    let root = decode_document(raw)?;
    if root.get("partial").and_then(Value::as_bool) != Some(false) {
        return Err(FetchFailure::Decode);
    }
    let dataset_rows = bounded_count(root.get("num_examples").ok_or(FetchFailure::Decode)?)?;
    let statistics = root
        .get("statistics")
        .and_then(Value::as_array)
        .ok_or(FetchFailure::Decode)?;
    let mut categories = statistics
        .iter()
        .filter(|entry| entry.get("column_name").and_then(Value::as_str) == Some("category"));
    let category = categories.next().ok_or(FetchFailure::Decode)?;
    if categories.next().is_some()
        || category.get("column_type").and_then(Value::as_str) != Some("string_label")
    {
        return Err(FetchFailure::Decode);
    }
    let frequencies = category
        .pointer("/column_statistics/frequencies")
        .and_then(Value::as_object)
        .ok_or(FetchFailure::Decode)?;
    let overall_rows = bounded_count(
        frequencies
            .get(arena::CATEGORY)
            .ok_or(FetchFailure::Decode)?,
    )?;
    let mut counted_rows = 0usize;
    for count in frequencies.values() {
        counted_rows = counted_rows
            .checked_add(bounded_count(count)?)
            .ok_or(FetchFailure::TooLarge)?;
        if counted_rows > dataset_rows {
            return Err(FetchFailure::Decode);
        }
    }
    if dataset_rows == 0 || overall_rows == 0 || overall_rows > dataset_rows {
        return Err(FetchFailure::Decode);
    }
    Ok(DatasetStatistics {
        dataset_rows,
        overall_rows,
    })
}

struct RowCollection {
    statistics: DatasetStatistics,
    overall_rows: Vec<Value>,
    row_indices: BTreeSet<u64>,
    scanned_rows: usize,
    downloaded_bytes: usize,
    pages: usize,
}

impl RowCollection {
    fn from_statistics(raw: &str) -> Result<Self, FetchFailure> {
        Ok(Self {
            statistics: parse_statistics(raw)?,
            overall_rows: Vec::new(),
            row_indices: BTreeSet::new(),
            scanned_rows: 0,
            downloaded_bytes: raw.len(),
            pages: 0,
        })
    }

    fn next_offset(&self) -> Result<usize, FetchFailure> {
        if self.pages >= MAX_PAGES || self.scanned_rows >= MAX_DATASET_ROWS {
            return Err(FetchFailure::TooLarge);
        }
        if self.scanned_rows >= self.statistics.dataset_rows {
            return Err(FetchFailure::Decode);
        }
        Ok(self.scanned_rows)
    }

    fn append(&mut self, raw: &str, offset: usize) -> Result<bool, FetchFailure> {
        if self.pages >= MAX_PAGES
            || self.downloaded_bytes.saturating_add(raw.len()) > MAX_DOCUMENT_BYTES
        {
            return Err(FetchFailure::TooLarge);
        }
        if offset != self.scanned_rows
            || offset >= self.statistics.dataset_rows
            || self.overall_rows.len() == self.statistics.overall_rows
        {
            return Err(FetchFailure::Decode);
        }
        let root = decode_document(raw)?;
        if root
            .get("offset")
            .is_some_and(|returned| returned.as_u64() != Some(offset as u64))
        {
            return Err(FetchFailure::Decode);
        }
        let total = bounded_count(root.get("num_rows_total").ok_or(FetchFailure::Decode)?)?;
        if total != self.statistics.dataset_rows {
            return Err(FetchFailure::Decode);
        }
        let rows = root
            .get("rows")
            .and_then(Value::as_array)
            .ok_or(FetchFailure::Decode)?;
        if rows.len() != PAGE_LENGTH.min(total - offset) {
            return Err(FetchFailure::Decode);
        }
        let mut page_indices = BTreeSet::new();
        let mut selected = Vec::new();
        for row in rows {
            let index = row
                .get("row_idx")
                .or_else(|| row.get("row_index"))
                .and_then(Value::as_u64)
                .ok_or(FetchFailure::Decode)?;
            if row.get("row_idx").is_some()
                && row.get("row_index").is_some()
                && row.get("row_index").and_then(Value::as_u64) != Some(index)
            {
                return Err(FetchFailure::Decode);
            }
            if row
                .get("truncated_cells")
                .is_some_and(|cells| cells.as_array().is_none_or(|cells| !cells.is_empty()))
                || self.row_indices.contains(&index)
                || !page_indices.insert(index)
            {
                return Err(FetchFailure::Decode);
            }
            let contents = row
                .get("row")
                .and_then(Value::as_object)
                .ok_or(FetchFailure::Decode)?;
            let category = contents
                .get("category")
                .and_then(Value::as_str)
                .ok_or(FetchFailure::Decode)?;
            if category == arena::CATEGORY {
                selected.push(row.clone());
            }
        }
        // Inspect the entire page before accepting a count match: an extra
        // overall row later in this same page invalidates the independent count.
        if self.overall_rows.len().saturating_add(selected.len()) > self.statistics.overall_rows {
            return Err(FetchFailure::Decode);
        }
        self.row_indices.extend(page_indices);
        self.overall_rows.extend(selected);
        self.scanned_rows += rows.len();
        self.downloaded_bytes += raw.len();
        self.pages += 1;
        Ok(self.overall_rows.len() == self.statistics.overall_rows)
    }

    fn finish(self, observed_date: NaiveDate) -> Result<String, FetchFailure> {
        if self.overall_rows.len() != self.statistics.overall_rows || self.overall_rows.is_empty() {
            return Err(FetchFailure::Decode);
        }
        let document =
            json!({"rows":self.overall_rows,"num_rows_total":self.statistics.overall_rows});
        let raw = serde_json::to_string(&document).map_err(|_| FetchFailure::Decode)?;
        if raw.len() > MAX_DOCUMENT_BYTES {
            return Err(FetchFailure::TooLarge);
        }
        arena::parse_arena_at(&raw, observed_date).map_err(|failure| match failure {
            ParseError::TooLarge | ParseError::CountExceeded => FetchFailure::TooLarge,
            _ => FetchFailure::Decode,
        })?;
        Ok(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statistics(dataset_rows: usize, overall_rows: usize) -> Value {
        json!({"num_examples":dataset_rows,"partial":false,"statistics":[{
            "column_name":"category","column_type":"string_label","column_statistics":{
                "frequencies":{"overall":overall_rows,"coding":dataset_rows.saturating_sub(overall_rows)}
            }
        }]})
    }

    fn page(offset: usize, dataset_rows: usize, overall_positions: &[usize]) -> Value {
        let count = PAGE_LENGTH.min(dataset_rows.saturating_sub(offset));
        json!({"num_rows_total":dataset_rows,"partial":false,"rows":(offset..offset + count).map(|index| {
            let overall_rank = overall_positions.iter().position(|position| *position == index).map(|rank| rank + 1);
            let rank = overall_rank.unwrap_or(1);
            json!({"row_idx":index,"row":{
                "model_name":format!("Model {index}"),"rank":rank,
                "rating":1500.0-rank as f64,"rating_lower":1490.0-rank as f64,
                "rating_upper":1510.0-rank as f64,"vote_count":10083.0,
                "category":if overall_rank.is_some(){"overall"}else{"coding"},
                "leaderboard_publish_date":"2026-10-22"
            },"truncated_cells":[]})
        }).collect::<Vec<_>>()})
    }

    fn collection(dataset_rows: usize, overall_rows: usize) -> RowCollection {
        RowCollection::from_statistics(&statistics(dataset_rows, overall_rows).to_string()).unwrap()
    }

    fn observed_date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 24).unwrap()
    }

    #[test]
    fn complete_statistics_count_finds_noncontiguous_categories() {
        let positions = [1, 103, 249];
        let mut rows = collection(250, positions.len());
        assert!(!rows
            .append(&page(0, 250, &positions).to_string(), 0)
            .unwrap());
        assert!(!rows
            .append(&page(100, 250, &positions).to_string(), 100)
            .unwrap());
        assert!(rows
            .append(&page(200, 250, &positions).to_string(), 200)
            .unwrap());
        let parsed = arena::parse_arena(&rows.finish(observed_date()).unwrap()).unwrap();
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(parsed.published_date, "2026-10-22");
    }

    #[test]
    fn independent_count_allows_completion_before_the_dataset_end() {
        let positions = [1, 103, 150];
        let mut rows = collection(500, positions.len());
        assert!(!rows
            .append(&page(0, 500, &positions).to_string(), 0)
            .unwrap());
        assert!(rows
            .append(&page(100, 500, &positions).to_string(), 100)
            .unwrap());
        assert_eq!(rows.scanned_rows, 200);
        assert_eq!(
            arena::parse_arena(&rows.finish(observed_date()).unwrap())
                .unwrap()
                .entries
                .len(),
            3
        );
    }

    #[test]
    fn extra_overall_rows_later_in_the_same_page_invalidate_completion() {
        let mut rows = collection(100, 1);
        assert_eq!(
            rows.append(&page(0, 100, &[1, 99]).to_string(), 0),
            Err(FetchFailure::Decode)
        );
        assert_eq!(rows.scanned_rows, 0);
        assert!(rows.overall_rows.is_empty());
    }

    #[test]
    fn incomplete_or_interrupted_collection_cannot_emit_a_cache_document() {
        let mut interrupted = collection(250, 2);
        interrupted
            .append(&page(0, 250, &[1, 249]).to_string(), 0)
            .unwrap();
        assert_eq!(
            interrupted.finish(observed_date()),
            Err(FetchFailure::Decode)
        );
        let mut exhausted = collection(150, 2);
        exhausted
            .append(&page(0, 150, &[1]).to_string(), 0)
            .unwrap();
        assert!(!exhausted
            .append(&page(100, 150, &[1]).to_string(), 100)
            .unwrap());
        assert_eq!(exhausted.next_offset(), Err(FetchFailure::Decode));
        assert_eq!(exhausted.finish(observed_date()), Err(FetchFailure::Decode));
    }

    #[test]
    fn partial_or_inconsistent_statistics_are_refused() {
        let mut partial = statistics(250, 3);
        partial["partial"] = json!(true);
        assert_eq!(
            parse_statistics(&partial.to_string()),
            Err(FetchFailure::Decode)
        );
        let mut absent = statistics(250, 3);
        absent.as_object_mut().unwrap().remove("partial");
        assert_eq!(
            parse_statistics(&absent.to_string()),
            Err(FetchFailure::Decode)
        );
        let mut duplicate = statistics(250, 3);
        let column = duplicate["statistics"][0].clone();
        duplicate["statistics"].as_array_mut().unwrap().push(column);
        assert_eq!(
            parse_statistics(&duplicate.to_string()),
            Err(FetchFailure::Decode)
        );
        let mut impossible = statistics(250, 3);
        impossible["statistics"][0]["column_statistics"]["frequencies"]["overall"] = json!(251);
        assert_eq!(
            parse_statistics(&impossible.to_string()),
            Err(FetchFailure::Decode)
        );
    }

    #[test]
    fn changed_dataset_totals_short_pages_and_wrong_cursors_are_refused() {
        let mut rows = collection(250, 2);
        assert_eq!(
            rows.append(&page(0, 251, &[1, 249]).to_string(), 0),
            Err(FetchFailure::Decode)
        );
        assert_eq!(
            rows.append(&page(0, 250, &[1, 249]).to_string(), 1),
            Err(FetchFailure::Decode)
        );
        let mut partial = page(0, 250, &[1, 249]);
        partial["rows"].as_array_mut().unwrap().pop();
        assert_eq!(
            rows.append(&partial.to_string(), 0),
            Err(FetchFailure::Decode)
        );
        assert_eq!(rows.scanned_rows, 0);
    }

    #[test]
    fn duplicate_indices_api_errors_partial_pages_and_truncation_are_refused() {
        let mut rows = collection(250, 2);
        rows.append(&page(0, 250, &[1, 249]).to_string(), 0)
            .unwrap();
        let mut duplicate = page(100, 250, &[1, 249]);
        duplicate["rows"][0]["row_idx"] = json!(0);
        assert_eq!(
            rows.append(&duplicate.to_string(), 100),
            Err(FetchFailure::Decode)
        );
        for field in ["error", "partial"] {
            let mut invalid = page(100, 250, &[1, 249]);
            invalid[field] = if field == "error" {
                json!("fixture API error")
            } else {
                json!(true)
            };
            assert_eq!(
                rows.append(&invalid.to_string(), 100),
                Err(FetchFailure::Decode)
            );
        }
        let mut truncated = page(100, 250, &[1, 249]);
        truncated["rows"][0]["truncated_cells"] = json!(["model_name"]);
        assert_eq!(
            rows.append(&truncated.to_string(), 100),
            Err(FetchFailure::Decode)
        );
    }

    #[test]
    fn mixed_publication_dates_and_future_boards_fail_final_validation() {
        let mut rows = collection(100, 2);
        let mut mixed = page(0, 100, &[1, 99]);
        mixed["rows"][99]["row"]["leaderboard_publish_date"] = json!("2026-10-21");
        rows.append(&mixed.to_string(), 0).unwrap();
        assert_eq!(rows.finish(observed_date()), Err(FetchFailure::Decode));
        let mut future = collection(1, 1);
        future.append(&page(0, 1, &[0]).to_string(), 0).unwrap();
        assert_eq!(
            future.finish(NaiveDate::from_ymd_opt(2026, 10, 21).unwrap()),
            Err(FetchFailure::Decode)
        );
    }

    #[test]
    fn cumulative_bytes_and_dataset_scan_limits_are_bounded() {
        assert_eq!(
            parse_statistics(&statistics(MAX_DATASET_ROWS + 1, 1).to_string()),
            Err(FetchFailure::TooLarge)
        );
        let mut rows = collection(1, 1);
        assert_eq!(
            rows.append(&" ".repeat(MAX_DOCUMENT_BYTES + 1), 0),
            Err(FetchFailure::TooLarge)
        );
        rows.downloaded_bytes = MAX_DOCUMENT_BYTES;
        assert_eq!(
            rows.append(&page(0, 1, &[0]).to_string(), 0),
            Err(FetchFailure::TooLarge)
        );
        let mut boundary = collection(MAX_DATASET_ROWS, 1);
        let positions = [MAX_DATASET_ROWS - 1];
        for offset in (0..MAX_DATASET_ROWS).step_by(PAGE_LENGTH) {
            let complete = boundary
                .append(
                    &page(offset, MAX_DATASET_ROWS, &positions).to_string(),
                    offset,
                )
                .unwrap();
            assert_eq!(complete, offset + PAGE_LENGTH == MAX_DATASET_ROWS);
        }
        assert_eq!(boundary.pages, MAX_PAGES);
        assert!(boundary.finish(observed_date()).is_ok());
    }

    #[test]
    fn fixed_statistics_and_rows_urls_have_no_filter_or_credentials() {
        let statistics = viewer_url("/statistics").unwrap();
        let rows = rows_url(100).unwrap();
        assert_eq!(statistics.path(), "/statistics");
        assert_eq!(rows.path(), "/rows");
        for url in [statistics, rows] {
            assert_eq!(url.scheme(), "https");
            assert_eq!(url.host_str(), Some("datasets-server.huggingface.co"));
            assert_eq!(url.username(), "");
            assert_eq!(url.password(), None);
            let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
            assert_eq!(query["dataset"], DATASET_ID);
            assert_eq!(query["config"], arena::SOURCE_CONFIG);
            assert_eq!(query["split"], arena::SOURCE_SPLIT);
            assert!(!query.contains_key("where"));
        }
        assert!(viewer_url("/filter").is_err());
        assert!(rows_url(MAX_DATASET_ROWS).is_err());
        assert!(rows_url(99).is_err());
    }
}
