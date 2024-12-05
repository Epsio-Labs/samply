use std::collections::HashMap;

use fxprof_processed_profile::{
    CategoryColor, CategoryHandle, CategoryPairHandle, LibMappings, Marker, MarkerFieldFormat,
    MarkerFieldSchema, MarkerGraph, MarkerGraphType, MarkerLocation, MarkerSchema,
    MarkerStaticField, MarkerTiming, MarkerTypeHandle, ProcessHandle, Profile, StaticSchemaMarker,
    StringHandle, ThreadHandle,
};

use super::counter_file::{Counter, CounterCategory};
use super::lib_mappings::{LibMappingInfo, LibMappingOpQueue, LibMappingsHierarchy};
use super::marker_file::{EventOrSpanMarker, MarkerData, MarkerSpan, MarkerStats, TracingTimings};
use super::stack_converter::StackConverter;
use super::stack_depth_limiting_frame_iter::StackDepthLimitingFrameIter;
use super::types::StackFrame;
use super::unresolved_samples::{
    SampleData, SampleOrMarker, UnresolvedSampleOrMarker, UnresolvedSamples, UnresolvedStacks,
};

#[derive(Debug, Clone)]
pub struct MarkerOnThread {
    pub thread_handle: ThreadHandle,
    pub event_or_span: EventOrSpanMarker,
}

#[derive(Debug, Clone)]
pub struct CounterOnThread {
    pub thread_handle: ThreadHandle,
    pub counter: Counter,
}

#[derive(Debug, Clone)]
pub enum RssStatMember {
    ResidentFileMappingPages,
    ResidentAnonymousPages,
    AnonymousSwapEntries,
    ResidentSharedMemoryPages,
}

#[derive(Debug, Clone)]
pub struct ProcessSampleData {
    unresolved_samples: UnresolvedSamples,
    regular_lib_mapping_op_queue: LibMappingOpQueue,
    jitdump_lib_mapping_op_queues: Vec<LibMappingOpQueue>,
    perf_map_mappings: Option<LibMappings<LibMappingInfo>>,
    markers: Vec<MarkerOnThread>,
    counters: Vec<CounterOnThread>,
    process: ProcessHandle,
}

impl ProcessSampleData {
    pub fn new(
        unresolved_samples: UnresolvedSamples,
        regular_lib_mapping_op_queue: LibMappingOpQueue,
        jitdump_lib_mapping_op_queues: Vec<LibMappingOpQueue>,
        perf_map_mappings: Option<LibMappings<LibMappingInfo>>,
        markers: Vec<MarkerOnThread>,
        counters: Vec<CounterOnThread>,
        process: ProcessHandle,
    ) -> Self {
        Self {
            unresolved_samples,
            regular_lib_mapping_op_queue,
            jitdump_lib_mapping_op_queues,
            perf_map_mappings,
            markers,
            counters,
            process,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.unresolved_samples.is_empty()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn flush_samples_to_profile(
        self,
        profile: &mut Profile,
        user_category: CategoryPairHandle,
        kernel_category: CategoryPairHandle,
        stack_frame_scratch_buf: &mut Vec<StackFrame>,
        stacks: &UnresolvedStacks,
    ) {
        let ProcessSampleData {
            unresolved_samples,
            regular_lib_mapping_op_queue,
            jitdump_lib_mapping_op_queues,
            perf_map_mappings,
            markers,
            counters,
            process,
        } = self;
        let mut lib_mappings_hierarchy = LibMappingsHierarchy::new(regular_lib_mapping_op_queue);
        for jitdump_lib_mapping_ops in jitdump_lib_mapping_op_queues {
            lib_mappings_hierarchy.add_jitdump_lib_mappings_ops(jitdump_lib_mapping_ops);
        }
        if let Some(perf_map_mappings) = perf_map_mappings {
            lib_mappings_hierarchy.add_perf_map_mappings(perf_map_mappings);
        }
        let mut stack_converter = StackConverter::new(user_category, kernel_category);
        let samples = unresolved_samples.into_inner();
        for sample in samples {
            lib_mappings_hierarchy.process_ops(sample.timestamp_mono);
            let UnresolvedSampleOrMarker {
                thread_handle,
                timestamp,
                stack,
                sample_or_marker,
                extra_label_frame,
                ..
            } = sample;

            stack_frame_scratch_buf.clear();
            stacks.convert_back(stack, stack_frame_scratch_buf);
            let frames = stack_converter.convert_stack(
                stack_frame_scratch_buf,
                &lib_mappings_hierarchy,
                extra_label_frame,
            );
            let frames = StackDepthLimitingFrameIter::new(profile, frames, user_category);
            match sample_or_marker {
                SampleOrMarker::Sample(SampleData { cpu_delta, weight }) => {
                    profile.add_sample(thread_handle, timestamp, frames, cpu_delta, weight);
                }
                SampleOrMarker::MarkerHandle(mh) => {
                    profile.set_marker_stack(thread_handle, mh, frames);
                }
            }
        }

        let mut category_handles = HashMap::<String, CategoryHandle>::new();
        let logging_category = profile.add_category("(Logging)", CategoryColor::Green);

        let mut span_marker_types: HashMap<String, MarkerTypeHandle> = HashMap::new();
        let mut event_marker_types: HashMap<String, MarkerTypeHandle> = HashMap::new();

        let mut stats = MarkerStats::new();
        for marker in markers {
            stats.process_span(&marker.event_or_span);
            let mut extra_fields: Vec<_> = marker
                .event_or_span
                .extra_fields
                .clone()
                .into_iter()
                .collect();
            extra_fields.sort_by_key(|(k, _)| k.clone());

            let (field_names, field_values): (Vec<_>, Vec<_>) = extra_fields.into_iter().unzip();
            let marker_typename = field_names.join("_");

            match &marker.event_or_span.marker_data {
                MarkerData::Event => {
                    let marker_type = event_marker_types
                        .entry(marker_typename.clone())
                        .or_insert_with(|| EventMarker::create_marker_type(profile, &field_names));

                    let span_marker = EventMarker::new(
                        profile,
                        &logging_category,
                        &marker,
                        marker_type,
                        &field_values,
                    );
                    profile.add_marker(
                        marker.thread_handle,
                        MarkerTiming::Instant(marker.event_or_span.start_time),
                        span_marker,
                    );
                }
                MarkerData::Span(span) => {
                    let marker_type = span_marker_types
                        .entry(marker_typename.clone())
                        .or_insert_with(|| {
                            SpanMarkerWithTimings::create_marker_type(profile, &field_names)
                        });

                    let span_marker = SpanMarkerWithTimings::new(
                        profile,
                        &marker,
                        span,
                        &mut category_handles,
                        marker_type,
                        &field_values,
                    );
                    profile.add_marker(
                        marker.thread_handle,
                        MarkerTiming::Interval(marker.event_or_span.start_time, span.end_time),
                        span_marker,
                    );
                }
            }
        }
        if !stats.is_empty() {
            stats.dump();
        }

        for CounterOnThread {
            counter,
            thread_handle,
        } in counters
        {
            match counter.category {
                CounterCategory::Custom => {
                    let marker_type = CustomGraphMarker::create_marker_type(profile, &counter);

                    for sample in counter.samples {
                        let marker = CustomGraphMarker::new(
                            profile.intern_string(&counter.name),
                            CategoryHandle::OTHER,
                            marker_type,
                            sample.value,
                        );

                        profile.add_marker(
                            thread_handle,
                            MarkerTiming::Instant(sample.timestamp),
                            marker,
                        );
                    }
                }
                _ => {
                    let counter_handle = profile.add_counter(
                        process,
                        &counter.name,
                        counter.category.into(),
                        &counter.description,
                        counter.color,
                    );

                    for sample in counter.samples {
                        profile.add_counter_sample(
                            counter_handle,
                            sample.timestamp,
                            sample.value,
                            sample.modification_count,
                        );
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct RssStatMarker {
    pub name: StringHandle,
    pub total_bytes: i64,
    pub delta_bytes: i64,
}

impl RssStatMarker {
    pub fn new(name: StringHandle, total_bytes: i64, delta_bytes: i64) -> Self {
        Self {
            name,
            total_bytes,
            delta_bytes,
        }
    }
}

impl StaticSchemaMarker for RssStatMarker {
    const UNIQUE_MARKER_TYPE_NAME: &'static str = "RSS Anon";

    fn schema() -> MarkerSchema {
        MarkerSchema {
            type_name: Self::UNIQUE_MARKER_TYPE_NAME.into(),
            locations: vec![MarkerLocation::MarkerChart, MarkerLocation::MarkerTable],
            chart_label: Some("{marker.data.totalBytes}".into()),
            tooltip_label: Some("{marker.data.totalBytes}".into()),
            table_label: Some(
                "Total: {marker.data.totalBytes}, delta: {marker.data.deltaBytes}".into(),
            ),
            fields: vec![
                MarkerFieldSchema {
                    key: "totalBytes".into(),
                    label: "Total bytes".into(),
                    format: MarkerFieldFormat::Bytes,
                    searchable: true,
                },
                MarkerFieldSchema {
                    key: "deltaBytes".into(),
                    label: "Delta".into(),
                    format: MarkerFieldFormat::Bytes,
                    searchable: true,
                },
            ],
            static_fields: vec![MarkerStaticField {
                label: "Description".into(),
                value: "Emitted when the kmem:rss_stat tracepoint is hit.".into(),
            }],
            graphs: vec![],
        }
    }

    fn name(&self, _profile: &mut Profile) -> StringHandle {
        self.name
    }

    fn category(&self, _profile: &mut Profile) -> CategoryHandle {
        CategoryHandle::OTHER
    }

    fn string_field_value(&self, _field_index: u32) -> StringHandle {
        unreachable!()
    }

    fn number_field_value(&self, field_index: u32) -> f64 {
        match field_index {
            0 => self.total_bytes as f64,
            1 => self.delta_bytes as f64,
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OtherEventMarker(pub StringHandle);

impl StaticSchemaMarker for OtherEventMarker {
    const UNIQUE_MARKER_TYPE_NAME: &'static str = "Other event";

    fn schema() -> MarkerSchema {
        MarkerSchema {
            type_name: Self::UNIQUE_MARKER_TYPE_NAME.into(),
            locations: vec![MarkerLocation::MarkerChart, MarkerLocation::MarkerTable],
            chart_label: None,
            tooltip_label: None,
            table_label: None,
            fields: vec![],
            static_fields: vec![MarkerStaticField {
                label: "Description".into(),
                value:
                    "Emitted for any records in a perf.data file which don't map to a known event."
                        .into(),
            }],
            graphs: vec![],
        }
    }

    fn name(&self, _profile: &mut Profile) -> StringHandle {
        self.0
    }

    fn category(&self, _profile: &mut Profile) -> CategoryHandle {
        CategoryHandle::OTHER
    }

    fn string_field_value(&self, _field_index: u32) -> StringHandle {
        unreachable!()
    }

    fn number_field_value(&self, _field_index: u32) -> f64 {
        unreachable!()
    }
}

#[derive(Debug, Clone)]
pub struct UserTimingMarker(pub StringHandle);

impl StaticSchemaMarker for UserTimingMarker {
    const UNIQUE_MARKER_TYPE_NAME: &'static str = "UserTiming";

    fn schema() -> MarkerSchema {
        MarkerSchema {
            type_name: Self::UNIQUE_MARKER_TYPE_NAME.into(),
            locations: vec![MarkerLocation::MarkerChart, MarkerLocation::MarkerTable],
            chart_label: Some("{marker.data.name}".into()),
            tooltip_label: Some("{marker.data.name}".into()),
            table_label: Some("{marker.data.name}".into()),
            fields: vec![MarkerFieldSchema {
                key: "name".into(),
                label: "Name".into(),
                format: MarkerFieldFormat::String,
                searchable: true,
            }],
            static_fields: vec![MarkerStaticField {
                label: "Description".into(),
                value: "Emitted for performance.mark and performance.measure.".into(),
            }],
            graphs: vec![],
        }
    }

    fn name(&self, profile: &mut Profile) -> StringHandle {
        profile.intern_string("UserTiming")
    }

    fn category(&self, _profile: &mut Profile) -> CategoryHandle {
        CategoryHandle::OTHER
    }

    fn string_field_value(&self, _field_index: u32) -> StringHandle {
        self.0
    }

    fn number_field_value(&self, _field_index: u32) -> f64 {
        unreachable!()
    }
}

pub struct SchedSwitchMarkerOnCpuTrack;

impl StaticSchemaMarker for SchedSwitchMarkerOnCpuTrack {
    const UNIQUE_MARKER_TYPE_NAME: &'static str = "sched_switch";

    fn schema() -> MarkerSchema {
        MarkerSchema {
            type_name: Self::UNIQUE_MARKER_TYPE_NAME.into(),
            locations: vec![MarkerLocation::MarkerChart, MarkerLocation::MarkerTable],
            chart_label: None,
            tooltip_label: None,
            table_label: None,
            fields: vec![],
            static_fields: vec![MarkerStaticField {
                label: "Description".into(),
                value: "Emitted just before a running thread gets moved off-cpu.".into(),
            }],
            graphs: vec![],
        }
    }

    fn name(&self, profile: &mut Profile) -> StringHandle {
        profile.intern_string("sched_switch")
    }

    fn category(&self, _profile: &mut Profile) -> CategoryHandle {
        CategoryHandle::OTHER
    }

    fn string_field_value(&self, _field_index: u32) -> StringHandle {
        unreachable!()
    }

    fn number_field_value(&self, _field_index: u32) -> f64 {
        unreachable!()
    }
}

#[derive(Debug, Clone)]
pub struct SchedSwitchMarkerOnThreadTrack {
    pub cpu: u32,
}

impl StaticSchemaMarker for SchedSwitchMarkerOnThreadTrack {
    const UNIQUE_MARKER_TYPE_NAME: &'static str = "sched_switch";

    fn schema() -> MarkerSchema {
        MarkerSchema {
            type_name: Self::UNIQUE_MARKER_TYPE_NAME.into(),
            locations: vec![MarkerLocation::MarkerChart, MarkerLocation::MarkerTable],
            chart_label: None,
            tooltip_label: None,
            table_label: None,
            fields: vec![MarkerFieldSchema {
                key: "cpu".into(),
                label: "cpu".into(),
                format: MarkerFieldFormat::Integer,
                searchable: true,
            }],
            static_fields: vec![MarkerStaticField {
                label: "Description".into(),
                value: "Emitted just before a running thread gets moved off-cpu.".into(),
            }],
            graphs: vec![],
        }
    }

    fn name(&self, profile: &mut Profile) -> StringHandle {
        profile.intern_string("sched_switch")
    }

    fn category(&self, _profile: &mut Profile) -> CategoryHandle {
        CategoryHandle::OTHER
    }

    fn string_field_value(&self, _field_index: u32) -> StringHandle {
        unreachable!()
    }

    fn number_field_value(&self, _field_index: u32) -> f64 {
        self.cpu.into()
    }
}

#[derive(Debug, Clone)]
pub struct SpanMarkerWithTimings {
    name: StringHandle,
    label: StringHandle,
    category: CategoryHandle,
    marker_type: MarkerTypeHandle,
    timings: TracingTimings,
    extra_fields: Vec<StringHandle>,
}

impl SpanMarkerWithTimings {
    pub fn create_marker_type(
        profile: &mut Profile,
        extra_field_names: &[String],
    ) -> MarkerTypeHandle {
        let mut all_fields = vec![
            MarkerFieldSchema {
                key: "time_idle".into(),
                label: "time_idle".into(),
                format: MarkerFieldFormat::Duration,
                searchable: true,
            },
            MarkerFieldSchema {
                key: "time_busy".into(),
                label: "time_busy".into(),
                format: MarkerFieldFormat::Duration,
                searchable: true,
            },
            MarkerFieldSchema {
                key: "name".into(),
                label: "name".into(),
                format: MarkerFieldFormat::String,
                searchable: true,
            },
        ];

        all_fields.extend(extra_field_names.iter().map(|name| MarkerFieldSchema {
            key: name.into(),
            label: name.into(),
            format: MarkerFieldFormat::String,
            searchable: true,
        }));

        profile.register_marker_type(MarkerSchema {
            type_name: format!("Span-{}", extra_field_names.join("_")),
            locations: vec![MarkerLocation::MarkerChart, MarkerLocation::MarkerTable],
            chart_label: Some("{marker.data.name}".into()),
            tooltip_label: Some("{marker.data.name}".into()),
            table_label: Some("{marker.data.name}".into()),
            fields: all_fields,
            static_fields: vec![],
            graphs: vec![],
        })
    }

    pub fn new(
        profile: &mut Profile,
        marker: &MarkerOnThread,
        span: &MarkerSpan,
        category_handles: &mut HashMap<String, CategoryHandle>,
        marker_type: &MarkerTypeHandle,
        field_values: &[String],
    ) -> Self {
        let marker = &marker.event_or_span;

        let label = if let Some(ref label) = span.profiler_label {
            profile.intern_string(label)
        } else {
            profile.intern_string(&span.span_type.to_string())
        };

        let category = *category_handles
            .entry(span.category.clone())
            .or_insert_with(|| profile.add_category(&span.category, CategoryColor::Green));

        let extra_fields = field_values
            .iter()
            .map(|value| profile.intern_string(value))
            .collect();

        Self {
            category,
            label,
            timings: span.timings.clone(),
            name: profile.intern_string(&marker.message),
            marker_type: *marker_type,
            extra_fields,
        }
    }
}

impl Marker for SpanMarkerWithTimings {
    fn marker_type(&self, _profile: &mut Profile) -> MarkerTypeHandle {
        self.marker_type
    }

    fn name(&self, _profile: &mut Profile) -> StringHandle {
        self.label
    }

    fn category(&self, _profile: &mut Profile) -> CategoryHandle {
        self.category
    }

    fn string_field_value(&self, field_index: u32) -> StringHandle {
        match field_index {
            2 => self.name,
            i => *self.extra_fields.get(i as usize - 3).unwrap(),
        }
    }

    fn number_field_value(&self, field_index: u32) -> f64 {
        match field_index {
            0 => self.timings.time_idle.as_micros() as f64 / 1000.0,
            1 => self.timings.time_busy.as_micros() as f64 / 1000.0,
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EventMarker {
    message: StringHandle,
    category: CategoryHandle,
    target: StringHandle,
    extra_fields: Vec<StringHandle>,
    marker_type: MarkerTypeHandle,
}

impl EventMarker {
    pub fn new(
        profile: &mut Profile,
        category: &CategoryHandle,
        marker: &MarkerOnThread,
        marker_type: &MarkerTypeHandle,
        field_values: &[String],
    ) -> Self {
        let marker = &marker.event_or_span;

        let extra_fields = field_values
            .iter()
            .map(|value| profile.intern_string(value))
            .collect();

        Self {
            category: *category,
            message: profile.intern_string(&marker.message),
            target: profile.intern_string(&marker.target),
            marker_type: *marker_type,
            extra_fields,
        }
    }

    pub fn create_marker_type(
        profile: &mut Profile,
        extra_field_names: &[String],
    ) -> MarkerTypeHandle {
        let mut all_fields = vec![MarkerFieldSchema {
            key: "message".into(),
            label: "Message".into(),
            format: MarkerFieldFormat::String,
            searchable: true,
        }];

        all_fields.extend(extra_field_names.iter().map(|name| MarkerFieldSchema {
            key: name.into(),
            label: name.into(),
            format: MarkerFieldFormat::String,
            searchable: true,
        }));

        profile.register_marker_type(MarkerSchema {
            type_name: format!("Event-{}", extra_field_names.join("_")),
            locations: vec![MarkerLocation::MarkerChart, MarkerLocation::MarkerTable],
            chart_label: Some("{marker.data.message}".into()),
            tooltip_label: Some("{marker.data.message}".into()),
            table_label: Some("{marker.data.message}".into()),
            fields: all_fields,
            static_fields: vec![],
            graphs: vec![],
        })
    }
}

impl Marker for EventMarker {
    fn marker_type(&self, _profile: &mut Profile) -> MarkerTypeHandle {
        self.marker_type
    }

    fn name(&self, _profile: &mut Profile) -> StringHandle {
        self.target
    }

    fn category(&self, _profile: &mut Profile) -> CategoryHandle {
        self.category
    }

    fn string_field_value(&self, field_index: u32) -> StringHandle {
        match field_index {
            0 => self.message,
            i => *self.extra_fields.get(i as usize - 1).unwrap(),
        }
    }

    fn number_field_value(&self, _field_index: u32) -> f64 {
        unreachable!()
    }
}

struct CustomGraphMarker {
    marker_type: MarkerTypeHandle,
    category: CategoryHandle,
    name: StringHandle,
    value: f64,
}

impl CustomGraphMarker {
    pub fn create_marker_type(profile: &mut Profile, counter: &Counter) -> MarkerTypeHandle {
        profile.register_marker_type(MarkerSchema {
            type_name: format!("CustomGraph-{}", counter.name),
            locations: vec![],
            chart_label: None,
            tooltip_label: None,
            table_label: None,
            fields: vec![MarkerFieldSchema {
                key: "value".into(),
                label: "Value".into(),
                format: MarkerFieldFormat::Decimal,
                searchable: false,
            }],
            static_fields: vec![],
            graphs: vec![MarkerGraph {
                key: "value".into(),
                graph_type: MarkerGraphType::Line,
                color: counter.color,
            }],
        })
    }

    pub fn new(
        name: StringHandle,
        category: CategoryHandle,
        marker_type: MarkerTypeHandle,
        value: f64,
    ) -> Self {
        Self {
            marker_type,
            category,
            name,
            value,
        }
    }
}

impl Marker for CustomGraphMarker {
    fn marker_type(&self, _profile: &mut Profile) -> MarkerTypeHandle {
        self.marker_type
    }

    fn name(&self, _profile: &mut Profile) -> StringHandle {
        self.name
    }

    fn category(&self, _profile: &mut Profile) -> CategoryHandle {
        self.category
    }

    fn string_field_value(&self, _field_index: u32) -> StringHandle {
        unreachable!()
    }

    fn number_field_value(&self, field_index: u32) -> f64 {
        match field_index {
            0 => self.value,
            _ => unreachable!(),
        }
    }
}
