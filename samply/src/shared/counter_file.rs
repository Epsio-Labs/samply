use std::fs::File;
use std::path::{Path, PathBuf};

use fxprof_processed_profile::{GraphColor, Timestamp};

use super::timestamp_converter::TimestampConverter;
use super::utils::open_file_with_fallback;

#[derive(Debug, Clone)]
pub enum CounterCategory {
    Memory,
    Bandwidth,
    Cpu,
    Custom,
}

impl From<&str> for CounterCategory {
    fn from(value: &str) -> Self {
        match value {
            "Memory" => CounterCategory::Memory,
            "Bandwidth" => CounterCategory::Bandwidth,
            "CPU" => CounterCategory::Cpu,
            "Custom" => CounterCategory::Custom,
            _ => panic!("Invalid counter category: {}", value),
        }
    }
}

impl From<CounterCategory> for &str {
    fn from(val: CounterCategory) -> Self {
        match val {
            CounterCategory::Memory => "Memory",
            CounterCategory::Bandwidth => "Bandwidth",
            CounterCategory::Cpu => "CPU",
            CounterCategory::Custom => "Custom",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CounterSample {
    pub timestamp: Timestamp,
    pub value: f64,
    pub modification_count: u32,
}

fn get_graph_color(color: &str) -> Option<GraphColor> {
    match color {
        "blue" => Some(GraphColor::Blue),
        "green" => Some(GraphColor::Green),
        "grey" => Some(GraphColor::Grey),
        "ink" => Some(GraphColor::Ink),
        "magenta" => Some(GraphColor::Magenta),
        "orange" => Some(GraphColor::Orange),
        "purple" => Some(GraphColor::Purple),
        "red" => Some(GraphColor::Red),
        "teal" => Some(GraphColor::Teal),
        "yellow" => Some(GraphColor::Yellow),
        "unspec" => None,
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct Counter {
    pub name: String,
    pub category: CounterCategory,
    pub description: String,
    pub color: Option<GraphColor>,
    pub samples: Vec<CounterSample>,
}

fn parse_counter_file(file: File, timestamp_converter: TimestampConverter) -> Counter {
    let json: serde_json::Value = serde_json::from_reader(&file).ok().unwrap();

    let mut samples = Vec::new();

    for sample in json["samples"].as_array().unwrap() {
        let sample = sample.as_array().unwrap();
        samples.push(CounterSample {
            timestamp: timestamp_converter.convert_time(sample[0].as_u64().unwrap()),
            value: sample[1].as_f64().unwrap(),
            modification_count: sample[2].as_u64().unwrap() as u32,
        });
    }

    Counter {
        name: json["name"].as_str().unwrap().into(),
        category: json["category"].as_str().unwrap().into(),
        description: json["description"].as_str().unwrap().into(),
        color: get_graph_color(json["color"].as_str().unwrap()),
        samples,
    }
}

pub fn get_counter(
    counter_file: &Path,
    lookup_dirs: &[PathBuf],
    timestamp_converter: TimestampConverter,
) -> Result<Counter, std::io::Error> {
    let (f, _true_path) = open_file_with_fallback(counter_file, lookup_dirs)?;
    Ok(parse_counter_file(f, timestamp_converter))
}
