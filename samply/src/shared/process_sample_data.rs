use std::collections::HashMap;

use fxprof_processed_profile::{
    Category, CategoryColor, CategoryHandle, LibMappings, Marker, MarkerFieldFlags,
    MarkerFieldFormat, MarkerLocations, MarkerTiming, MarkerTypeHandle, Profile,
    RuntimeSchemaMarkerField, RuntimeSchemaMarkerSchema, StaticSchemaMarker,
    StaticSchemaMarkerField, StringHandle, SubcategoryHandle, ThreadHandle,
};

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
}

impl ProcessSampleData {
    pub fn new(
        unresolved_samples: UnresolvedSamples,
        regular_lib_mapping_op_queue: LibMappingOpQueue,
        jitdump_lib_mapping_op_queues: Vec<LibMappingOpQueue>,
        perf_map_mappings: Option<LibMappings<LibMappingInfo>>,
        markers: Vec<MarkerOnThread>,
    ) -> Self {
        Self {
            unresolved_samples,
            regular_lib_mapping_op_queue,
            jitdump_lib_mapping_op_queues,
            perf_map_mappings,
            markers,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.unresolved_samples.is_empty()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn flush_samples_to_profile(
        self,
        profile: &mut Profile,
        user_category: SubcategoryHandle,
        kernel_category: SubcategoryHandle,
        stack_frame_scratch_buf: &mut Vec<StackFrame>,
        stacks: &UnresolvedStacks,
    ) {
        let ProcessSampleData {
            unresolved_samples,
            regular_lib_mapping_op_queue,
            jitdump_lib_mapping_op_queues,
            perf_map_mappings,
            markers,
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
                thread_handle,
                stack_frame_scratch_buf,
                &lib_mappings_hierarchy,
                extra_label_frame,
            );
            let mut frames =
                StackDepthLimitingFrameIter::new(profile, frames, thread_handle, user_category);
            let stack_handle =
                profile.handle_for_stack_frames(thread_handle, move |p| frames.next(p));
            match sample_or_marker {
                SampleOrMarker::Sample(SampleData { cpu_delta, weight }) => {
                    profile.add_sample(thread_handle, timestamp, stack_handle, cpu_delta, weight);
                }
                SampleOrMarker::MarkerHandle(mh) => {
                    profile.set_marker_stack(thread_handle, mh, stack_handle);
                }
            }
        }

        let mut category_handles = HashMap::<String, CategoryHandle>::new();
        let logging_category =
            profile.handle_for_category(Category("(Logging)", CategoryColor::Green));

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
                        .or_insert_with(|| {
                            EventMarker::create_marker_type(
                                profile,
                                &field_names,
                                &logging_category,
                            )
                        });

                    let span_marker =
                        EventMarker::new(profile, &marker, marker_type, &field_values);
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
                            SpanMarkerWithTimings::create_marker_type(
                                profile,
                                &field_names,
                                span,
                                &mut category_handles,
                            )
                        });

                    let span_marker = SpanMarkerWithTimings::new(
                        profile,
                        &marker,
                        span,
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

    const CHART_LABEL: Option<&'static str> = Some("{marker.data.totalBytes}");
    const TOOLTIP_LABEL: Option<&'static str> = Some("{marker.data.totalBytes}");
    const TABLE_LABEL: Option<&'static str> =
        Some("Total: {marker.data.totalBytes}, delta: {marker.data.deltaBytes}");

    const DESCRIPTION: Option<&'static str> =
        Some("Emitted when the kmem:rss_stat tracepoint is hit.");

    const FIELDS: &'static [StaticSchemaMarkerField] = &[
        StaticSchemaMarkerField {
            key: "totalBytes",
            label: "Total bytes",
            format: MarkerFieldFormat::Bytes,
            flags: MarkerFieldFlags::SEARCHABLE,
        },
        StaticSchemaMarkerField {
            key: "deltaBytes",
            label: "Delta",
            format: MarkerFieldFormat::Bytes,
            flags: MarkerFieldFlags::SEARCHABLE,
        },
    ];

    fn name(&self, _profile: &mut Profile) -> StringHandle {
        self.name
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

    fn flow_field_value(&self, _field_index: u32) -> u64 {
        unreachable!()
    }
}

#[derive(Debug, Clone)]
pub struct OtherEventMarker(pub StringHandle);

impl StaticSchemaMarker for OtherEventMarker {
    const UNIQUE_MARKER_TYPE_NAME: &'static str = "Other event";

    const DESCRIPTION: Option<&'static str> =
        Some("Emitted for any records in a perf.data file which don't map to a known event.");

    const FIELDS: &'static [fxprof_processed_profile::StaticSchemaMarkerField] = &[];

    fn name(&self, _profile: &mut Profile) -> StringHandle {
        self.0
    }

    fn string_field_value(&self, _field_index: u32) -> StringHandle {
        unreachable!()
    }

    fn number_field_value(&self, _field_index: u32) -> f64 {
        unreachable!()
    }

    fn flow_field_value(&self, _field_index: u32) -> u64 {
        unreachable!()
    }
}

#[derive(Debug, Clone)]
pub struct UserTimingMarker(pub StringHandle);

impl StaticSchemaMarker for UserTimingMarker {
    const UNIQUE_MARKER_TYPE_NAME: &'static str = "UserTiming";

    const DESCRIPTION: Option<&'static str> =
        Some("Emitted for performance.mark and performance.measure.");

    const CHART_LABEL: Option<&'static str> = Some("{marker.data.name}");
    const TOOLTIP_LABEL: Option<&'static str> = Some("{marker.data.name}");
    const TABLE_LABEL: Option<&'static str> = Some("{marker.data.name}");

    const FIELDS: &'static [StaticSchemaMarkerField] = &[StaticSchemaMarkerField {
        key: "name",
        label: "Name",
        format: MarkerFieldFormat::String,
        flags: MarkerFieldFlags::SEARCHABLE,
    }];

    fn name(&self, profile: &mut Profile) -> StringHandle {
        profile.handle_for_string("UserTiming")
    }

    fn string_field_value(&self, _field_index: u32) -> StringHandle {
        self.0
    }

    fn number_field_value(&self, _field_index: u32) -> f64 {
        unreachable!()
    }

    fn flow_field_value(&self, _field_index: u32) -> u64 {
        unreachable!()
    }
}

pub struct SchedSwitchMarkerOnCpuTrack;

impl StaticSchemaMarker for SchedSwitchMarkerOnCpuTrack {
    const UNIQUE_MARKER_TYPE_NAME: &'static str = "sched_switch";

    const DESCRIPTION: Option<&'static str> =
        Some("Emitted just before a running thread gets moved off-cpu.");

    const FIELDS: &'static [StaticSchemaMarkerField] = &[];

    fn name(&self, profile: &mut Profile) -> StringHandle {
        profile.handle_for_string("sched_switch")
    }

    fn string_field_value(&self, _field_index: u32) -> StringHandle {
        unreachable!()
    }

    fn number_field_value(&self, _field_index: u32) -> f64 {
        unreachable!()
    }

    fn flow_field_value(&self, _field_index: u32) -> u64 {
        unreachable!()
    }
}

#[derive(Debug, Clone)]
pub struct SchedSwitchMarkerOnThreadTrack {
    pub cpu: u32,
}

impl StaticSchemaMarker for SchedSwitchMarkerOnThreadTrack {
    const UNIQUE_MARKER_TYPE_NAME: &'static str = "sched_switch";

    const DESCRIPTION: Option<&'static str> =
        Some("Emitted just before a running thread gets moved off-cpu.");

    const FIELDS: &'static [StaticSchemaMarkerField] = &[StaticSchemaMarkerField {
        key: "cpu",
        label: "cpu",
        format: MarkerFieldFormat::Integer,
        flags: MarkerFieldFlags::SEARCHABLE,
    }];

    fn name(&self, profile: &mut Profile) -> StringHandle {
        profile.handle_for_string("sched_switch")
    }

    fn string_field_value(&self, _field_index: u32) -> StringHandle {
        unreachable!()
    }

    fn number_field_value(&self, _field_index: u32) -> f64 {
        self.cpu.into()
    }

    fn flow_field_value(&self, _field_index: u32) -> u64 {
        unreachable!()
    }
}

#[derive(Debug, Clone)]
pub struct SpanMarkerWithTimings {
    name: StringHandle,
    label: StringHandle,
    marker_type: MarkerTypeHandle,
    timings: TracingTimings,
    extra_fields: Vec<StringHandle>,
}

impl SpanMarkerWithTimings {
    pub fn create_marker_type(
        profile: &mut Profile,
        extra_field_names: &[String],
        span: &MarkerSpan,
        category_handles: &mut HashMap<String, CategoryHandle>,
    ) -> MarkerTypeHandle {
        let mut all_fields = vec![
            RuntimeSchemaMarkerField {
                key: "time_idle".into(),
                label: "time_idle".into(),
                format: MarkerFieldFormat::Duration,
                flags: MarkerFieldFlags::SEARCHABLE,
            },
            RuntimeSchemaMarkerField {
                key: "time_busy".into(),
                label: "time_busy".into(),
                format: MarkerFieldFormat::Duration,
                flags: MarkerFieldFlags::SEARCHABLE,
            },
            RuntimeSchemaMarkerField {
                key: "name".into(),
                label: "name".into(),
                format: MarkerFieldFormat::String,
                flags: MarkerFieldFlags::SEARCHABLE,
            },
        ];

        all_fields.extend(
            extra_field_names
                .iter()
                .map(|name| RuntimeSchemaMarkerField {
                    key: name.into(),
                    label: name.into(),
                    format: MarkerFieldFormat::String,
                    flags: MarkerFieldFlags::SEARCHABLE,
                }),
        );

        let category = *category_handles
            .entry(span.category.clone())
            .or_insert_with(|| {
                profile.handle_for_category(Category(&span.category, CategoryColor::Green))
            });

        profile.register_marker_type(RuntimeSchemaMarkerSchema {
            description: None,
            type_name: format!("Span-{}", extra_field_names.join("_")),
            locations: MarkerLocations::MARKER_CHART | MarkerLocations::MARKER_TABLE,
            chart_label: Some("{marker.data.name}".into()),
            tooltip_label: Some("{marker.data.name}".into()),
            table_label: Some("{marker.data.name}".into()),
            fields: all_fields,
            category,
            graphs: vec![],
        })
    }

    pub fn new(
        profile: &mut Profile,
        marker: &MarkerOnThread,
        span: &MarkerSpan,
        marker_type: &MarkerTypeHandle,
        field_values: &[String],
    ) -> Self {
        let marker = &marker.event_or_span;

        let label = if let Some(ref label) = span.profiler_label {
            profile.handle_for_string(label)
        } else {
            profile.handle_for_string(&span.span_type.to_string())
        };

        let extra_fields = field_values
            .iter()
            .map(|value| profile.handle_for_string(value))
            .collect();

        Self {
            label,
            timings: span.timings.clone(),
            name: profile.handle_for_string(&marker.message),
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
    target: StringHandle,
    extra_fields: Vec<StringHandle>,
    marker_type: MarkerTypeHandle,
}

impl EventMarker {
    pub fn new(
        profile: &mut Profile,
        marker: &MarkerOnThread,
        marker_type: &MarkerTypeHandle,
        field_values: &[String],
    ) -> Self {
        let marker = &marker.event_or_span;

        let extra_fields = field_values
            .iter()
            .map(|value| profile.handle_for_string(value))
            .collect();

        Self {
            message: profile.handle_for_string(&marker.message),
            target: profile.handle_for_string(&marker.target),
            marker_type: *marker_type,
            extra_fields,
        }
    }

    pub fn create_marker_type(
        profile: &mut Profile,
        extra_field_names: &[String],
        category: &CategoryHandle,
    ) -> MarkerTypeHandle {
        let mut all_fields = vec![RuntimeSchemaMarkerField {
            key: "message".into(),
            label: "Message".into(),
            format: MarkerFieldFormat::String,
            flags: MarkerFieldFlags::SEARCHABLE,
        }];

        all_fields.extend(
            extra_field_names
                .iter()
                .map(|name| RuntimeSchemaMarkerField {
                    key: name.into(),
                    label: name.into(),
                    format: MarkerFieldFormat::String,
                    flags: MarkerFieldFlags::SEARCHABLE,
                }),
        );

        profile.register_marker_type(RuntimeSchemaMarkerSchema {
            description: None,
            type_name: format!("Event-{}", extra_field_names.join("_")),
            locations: MarkerLocations::MARKER_CHART | MarkerLocations::MARKER_TABLE,
            chart_label: Some("{marker.data.message}".into()),
            tooltip_label: Some("{marker.data.message}".into()),
            table_label: Some("{marker.data.message}".into()),
            fields: all_fields,
            category: *category,
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

    fn string_field_value(&self, field_index: u32) -> StringHandle {
        match field_index {
            0 => self.message,
            i => *self.extra_fields.get(i as usize - 1).unwrap(),
        }
    }

    fn number_field_value(&self, _field_index: u32) -> f64 {
        unreachable!()
    }

    fn flow_field_value(&self, _field_index: u32) -> u64 {
        unreachable!()
    }
}
