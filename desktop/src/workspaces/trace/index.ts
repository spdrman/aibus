// Trace workspace (B1.1 spike, #304): the execution graph, the timeline and the inspector over a
// projection the Rust host sends. Later trace slices of EPIC B grow here.
export { TraceContext, useTrace, type TraceState } from "./context";
export { GraphView } from "./GraphView";
export { TimelineView } from "./TimelineView";
export { InspectorView } from "./InspectorView";
export { layout } from "./layout";
export { TraceWorkspace } from "./TraceWorkspace";
