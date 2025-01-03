use std::collections::HashMap;
use std::fmt::Display;
use std::fs::File;
use std::io::{BufRead, BufReader, Lines};
use std::ops::AddAssign;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fxprof_processed_profile::Timestamp;
use log::warn;

use super::timestamp_converter::TimestampConverter;
use super::utils::open_file_with_fallback;

#[derive(Debug, Default, Clone)]
pub struct TracingTimings {
    pub time_busy: Duration,
    pub time_idle: Duration,
}

impl AddAssign for TracingTimings {
    fn add_assign(&mut self, other: Self) {
        *self += &other;
    }
}

impl AddAssign<&Self> for TracingTimings {
    fn add_assign(&mut self, other: &Self) {
        self.time_busy += other.time_busy;
        self.time_idle += other.time_idle;
    }
}

#[derive(Debug, Clone)]
pub struct EventOrSpanMarker {
    pub start_time: Timestamp,
    pub message: String,
    pub target: String,
    pub extra_fields: HashMap<String, String>,
    pub marker_data: MarkerData,
}

#[derive(Debug, Clone)]
pub enum MarkerData {
    Span(MarkerSpan),
    Event,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpanType {
    Total,
    Running,
}

impl Display for SpanType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpanType::Running => write!(f, "Running"),
            SpanType::Total => write!(f, "Total"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MarkerSpan {
    pub span_type: SpanType,
    pub end_time: Timestamp,
    pub timings: TracingTimings,
    pub category: String,
    pub profiler_label: Option<String>,
    pub stats_label: Option<String>,
}

pub struct MarkerStats {
    per_collection_map: HashMap<String, TracingTimings>,
}

impl MarkerStats {
    pub fn new() -> Self {
        Self {
            per_collection_map: HashMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.per_collection_map.is_empty()
    }

    pub fn process_span(&mut self, marker: &EventOrSpanMarker) {
        if let MarkerData::Span(span) = &marker.marker_data {
            if span.span_type != SpanType::Total {
                return;
            }
            if let Some(label) = &span.stats_label {
                *self.per_collection_map.entry(label.clone()).or_default() += &span.timings;
            }
        }
    }

    fn calc_per_type(&self) -> HashMap<String, TracingTimings> {
        let mut per_type = HashMap::new();
        for (collection, timings) in self.per_collection_map.iter() {
            let (collection_type, _) = collection.split_once('-').unwrap();
            *per_type.entry(collection_type.to_string()).or_default() += timings;
        }
        per_type
    }

    fn dump_stat(
        &self,
        title: &str,
        timings_map: &HashMap<String, TracingTimings>,
        callback: fn(&TracingTimings) -> Duration,
    ) {
        let mut timings: Vec<(_, _)> = timings_map.iter().map(|(k, v)| (k, callback(v))).collect();
        timings.sort_by_key(|(_, v)| v.as_nanos());
        timings.reverse();

        // TODO: better formatting? json? dump to file?
        println!("\t{}:", title);
        for (k, v) in timings {
            println!("\t\t{:<40}\t{:?}", k, v);
        }
    }

    fn dump_stats_map(&self, title: &str, timings_map: &HashMap<String, TracingTimings>) {
        println!("{}:", title);

        self.dump_stat("Total", timings_map, |t| t.time_busy + t.time_idle);
        self.dump_stat("Busy", timings_map, |t| t.time_busy);
        self.dump_stat("Idle", timings_map, |t| t.time_idle);
    }

    pub fn dump(&self) {
        let per_type_map = self.calc_per_type();
        self.dump_stats_map("Per Type", &per_type_map);
        self.dump_stats_map("Per Collection", &self.per_collection_map);
    }
}

struct SpanTracker {
    start_keyword: String,
    end_keyword: String,
    started_span_cache: HashMap<u64, serde_json::Value>,
}

impl SpanTracker {
    fn new(start_keyword: &str, end_keyword: &str) -> Self {
        Self {
            start_keyword: start_keyword.to_string(),
            end_keyword: end_keyword.to_string(),
            started_span_cache: HashMap::new(),
        }
    }

    fn process_line(
        &mut self,
        id: u64,
        json: serde_json::Value,
    ) -> Option<(serde_json::Value, serde_json::Value)> {
        let message = json.get("fields")?.get("message")?.as_str()?.to_string();
        if message != self.start_keyword && message != self.end_keyword {
            return None;
        }

        let span_exists = self.started_span_cache.contains_key(&id);
        let expected_keyword = if span_exists {
            &self.end_keyword
        } else {
            &self.start_keyword
        };

        if &message != expected_keyword {
            warn!(
                "Dropping span - expected '{}', got '{}' for span {:?}",
                expected_keyword, message, json
            );
            return None;
        }

        if span_exists {
            Some((self.started_span_cache.remove(&id)?, json))
        } else {
            self.started_span_cache.insert(id, json);
            None
        }
    }
}

pub struct MarkerFile {
    lines: Lines<BufReader<File>>,
    timestamp_converter: TimestampConverter,
    new_close_tracker: SpanTracker,
    enter_exit_tracker: SpanTracker,
}

impl MarkerFile {
    pub fn parse(file: File, timestamp_converter: TimestampConverter) -> Self {
        Self {
            lines: BufReader::new(file).lines(),
            timestamp_converter,
            new_close_tracker: SpanTracker::new("new", "close"),
            enter_exit_tracker: SpanTracker::new("enter", "exit"),
        }
    }
}

fn parse_timing_field(fields: &serde_json::Value, field: &str) -> Option<Duration> {
    let field_str = fields.get(field)?.as_str().unwrap().replace('µ', "u");

    let end_idx = field_str
        .rfind(|c| char::is_numeric(c) || c == '.')
        .unwrap();
    let (num, unit) = field_str.split_at(end_idx + 1);

    Some(match unit {
        "s" => Duration::from_secs_f64(num.parse().unwrap()),
        "ms" => Duration::from_secs_f64(num.parse::<f64>().unwrap() / 1_000.0),
        "us" => Duration::from_secs_f64(num.parse::<f64>().unwrap() / 1_000_000.0),
        "ns" => Duration::from_secs_f64(num.parse::<f64>().unwrap() / 1_000_000_000.0),
        _ => panic!("unknown unit '{}' in field {}", unit, field_str),
    })
}

impl MarkerFile {
    fn read_timestamp_from_event(&self, json: &serde_json::Value) -> u64 {
        json.get("timestamp")
            .unwrap()
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
    }

    fn value_to_hashmap(value: &serde_json::Value) -> HashMap<String, String> {
        value
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    match v.as_str() {
                        Some(s) => s.to_string(),
                        None => v.to_string(),
                    },
                )
            })
            .collect::<HashMap<String, String>>()
    }

    fn process_complete_span(
        &mut self,
        span_type: SpanType,
        start: serde_json::Value,
        end: serde_json::Value,
    ) -> Option<EventOrSpanMarker> {
        let fields = end.get("fields").unwrap();

        let start_time = self.read_timestamp_from_event(&start);
        let end_time = self.read_timestamp_from_event(&end);

        let mut extra_fields = Self::value_to_hashmap(end.get("span").unwrap());

        let message = extra_fields.remove("name").unwrap();
        let action = extra_fields.get("action").map_or("-", String::as_str);

        // TODO: get label+category from sampled program?
        // Expected format: AtomType[-AtomId]/CollectionType-CollectionID
        let (category, profiler_label, stats_label) =
            if let Some((atom, collection)) = action.split_once('/') {
                let (collection_type, mut id) = collection
                    .split_once('-')
                    .unwrap_or_else(|| panic!("Invalid collection: {}", collection));
                if id.len() > 8 {
                    id = &id[..8];
                }

                let profiler_label = format!("{}-{} {}", collection_type, &id, span_type);
                let stats_label = format!("{}::{}-{}", collection_type, message, &id);
                (atom.to_string(), Some(profiler_label), Some(stats_label))
            } else {
                (action.to_string(), None, None)
            };

        let target = end.get("target").unwrap().as_str().unwrap().to_string();

        let time_busy = parse_timing_field(fields, "time.busy")
            .unwrap_or(Duration::from_nanos(end_time - start_time));
        let time_idle = parse_timing_field(fields, "time.idle").unwrap_or_default();

        Some(EventOrSpanMarker {
            start_time: self.timestamp_converter.convert_time(start_time),
            message,
            target,
            extra_fields,
            marker_data: MarkerData::Span(MarkerSpan {
                end_time: self.timestamp_converter.convert_time(end_time),
                span_type,
                category,
                profiler_label,
                stats_label,
                timings: TracingTimings {
                    time_busy,
                    time_idle,
                },
            }),
        })
    }

    fn process_event(&mut self, event: serde_json::Value) -> Option<EventOrSpanMarker> {
        let start_time = self
            .timestamp_converter
            .convert_time(self.read_timestamp_from_event(&event));
        let target = event.get("target").unwrap().as_str().unwrap().to_string();

        let mut extra_fields = Self::value_to_hashmap(event.get("fields").unwrap());
        let message = extra_fields.remove("message")?;

        Some(EventOrSpanMarker {
            start_time,
            message,
            target,
            extra_fields,
            marker_data: MarkerData::Event,
        })
    }

    fn process_line(&mut self, line: &str) -> Option<EventOrSpanMarker> {
        let (ids, json) = line.split_once(' ')?;
        let json: serde_json::Value = serde_json::from_str(json).ok()?;

        let (id, tid) = if let Some((id, tid)) = ids.split_once(',') {
            (id.parse::<u64>().ok()?, Some(tid.parse::<i32>().ok()?))
        } else {
            (ids.parse::<u64>().ok()?, None)
        };

        if id != 0 {
            if let Some((start, end)) = self.new_close_tracker.process_line(id, json.clone()) {
                self.process_complete_span(SpanType::Total, start, end)
            } else if let Some((start, mut end)) = self.enter_exit_tracker.process_line(id, json) {
                // tid only makes sense for running spans
                if let Some(tid) = tid {
                    end["span"]["tid"] = serde_json::Value::from(tid);
                }
                self.process_complete_span(SpanType::Running, start, end)
            } else {
                None
            }
        } else {
            self.process_event(json)
        }
    }
}

impl Iterator for MarkerFile {
    type Item = EventOrSpanMarker;

    fn next(&mut self) -> Option<Self::Item> {
        while let Ok(line) = self.lines.next()? {
            if let Some(marker) = self.process_line(&line) {
                return Some(marker);
            }
        }
        None
    }
}

pub struct MarkerFileInfo {
    #[allow(dead_code)]
    pub prefix: String,
    #[allow(dead_code)]
    pub pid: u32,
    #[allow(dead_code)]
    pub tid: Option<u32>,
}

#[allow(unused)]
pub fn parse_marker_file_path(path: &Path) -> MarkerFileInfo {
    let filename = path.file_name().unwrap().to_str().unwrap();
    // strip .txt extension
    let filename = &filename[..filename.len() - 4];
    let mut parts = filename.splitn(3, '-');
    let prefix = parts.next().unwrap().to_owned();
    let pid = parts.next().unwrap().parse().unwrap();
    let tid = parts.next().map(|tid| tid.parse().unwrap());
    MarkerFileInfo { prefix, pid, tid }
}

pub fn get_markers(
    marker_file: &Path,
    lookup_dirs: &[PathBuf],
    timestamp_converter: TimestampConverter,
) -> Result<Vec<EventOrSpanMarker>, std::io::Error> {
    let (f, _true_path) = open_file_with_fallback(marker_file, lookup_dirs)?;
    let marker_file = MarkerFile::parse(f, timestamp_converter);
    let mut marker_spans: Vec<EventOrSpanMarker> = marker_file.collect();
    marker_spans.sort_by_key(|m| m.start_time);
    Ok(marker_spans)
}
