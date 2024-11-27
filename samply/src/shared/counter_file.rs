use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use fxprof_processed_profile::{GraphColor, Timestamp};

use super::timestamp_converter::TimestampConverter;
use super::utils::open_file_with_fallback;

#[derive(Debug, Clone)]
pub struct CounterSample {
    pub timestamp: Timestamp,
    pub value_delta: f64,
    pub number_of_operations_delta: u32,
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
    pub category: String,
    pub description: String,
    pub color: Option<GraphColor>,
    pub samples: Vec<CounterSample>,
}

fn process_counter_definition_line(line: &str) -> Option<Counter> {
    let mut split = line.splitn(4, ',');
    let category = split.next()?;
    let name = split.next()?;
    let description = split.next()?.to_owned();
    let color = split.next()?.to_owned();
    Some(Counter {
        name: name.to_owned(),
        category: category.to_owned(),
        description: description.to_owned(),
        color: get_graph_color(&color),
        samples: Vec::new(),
    })
}

fn process_counter_line(
    line: &str,
    timestamp_converter: &TimestampConverter,
) -> Option<CounterSample> {
    let mut split = line.splitn(3, ',');
    let timestamp = split.next()?;
    let value_delta = split.next()?;
    let number_of_operations_delta = split.next()?;
    let timestamp = timestamp_converter.convert_time(timestamp.parse::<u64>().ok()?);
    Some(CounterSample {
        timestamp,
        value_delta: value_delta.parse().ok()?,
        number_of_operations_delta: number_of_operations_delta.parse().ok()?,
    })
}

fn parse_counter_file(file: File, timestamp_converter: TimestampConverter) -> Option<Counter> {
    let mut lines = BufReader::new(file).lines();

    let definition_line = lines.next()?.ok()?;
    let mut counter = process_counter_definition_line(&definition_line)?;

    for line in lines {
        let line = line.ok()?;
        let sample = process_counter_line(&line, &timestamp_converter)?;
        counter.samples.push(sample);
    }

    Some(counter)
}

pub fn get_counter(
    counter_file: &Path,
    lookup_dirs: &[PathBuf],
    timestamp_converter: TimestampConverter,
) -> Result<Counter, std::io::Error> {
    let (f, _true_path) = open_file_with_fallback(counter_file, lookup_dirs)?;
    Ok(parse_counter_file(f, timestamp_converter).unwrap())
}
