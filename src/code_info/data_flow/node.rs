use core::panic;

use crate::{
    code_location::HPos,
    taint::{SinkType, SourceType},
};
use function_context::method_identifier::MethodIdentifier;
use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VariableSourceKind {
    Default,
    PrivateParam,
    NonPrivateParam,
    InoutParam,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataFlowNode {
    Vertex {
        id: String,
        unspecialized_id: Option<String>,
        label: String,
        pos: Option<HPos>,
        specialization_key: Option<String>,
    },
    VariableUseSource {
        kind: VariableSourceKind,
        id: String,
        pos: HPos,
    },
    VariableUseSink {
        id: String,
        pos: HPos,
    },
    TaintSource {
        id: String,
        label: String,
        pos: Option<HPos>,
        types: FxHashSet<SourceType>,
    },
    TaintSink {
        id: String,
        label: String,
        pos: Option<HPos>,
        types: FxHashSet<SinkType>,
    },
}

impl DataFlowNode {
    pub fn new(
        id: String,
        label: String,
        pos: Option<HPos>,
        specialization_key: Option<String>,
    ) -> Self {
        let mut id = id;
        let mut unspecialized_id = None;

        if let Some(specialization_key) = &specialization_key {
            unspecialized_id = Some(id.clone());
            id += "-";
            id += specialization_key.as_str();
        }

        DataFlowNode::Vertex {
            id,
            unspecialized_id,
            label,
            pos,
            specialization_key,
        }
    }

    pub fn get_for_method_argument(
        method_id: String,
        argument_offset: usize,
        arg_location: Option<HPos>,
        pos: Option<HPos>,
    ) -> Self {
        let arg_id = method_id.clone() + "#" + (argument_offset + 1).to_string().as_str();

        let mut specialization_key = None;

        if let Some(pos) = pos {
            specialization_key = Some(format!("{}:{}", pos.file_path, pos.start_offset));
        }

        Self::new(arg_id.clone(), arg_id, arg_location, specialization_key)
    }

    pub fn get_for_method_argument_out(
        method_id: String,
        argument_offset: usize,
        arg_location: Option<HPos>,
        pos: Option<HPos>,
    ) -> Self {
        let arg_id = "out ".to_string() + method_id.as_str() + "#" + (argument_offset + 1).to_string().as_str();

        let mut specialization_key = None;

        if let Some(pos) = pos {
            specialization_key = Some(format!("{}:{}", pos.file_path, pos.start_offset));
        }

        Self::new(arg_id.clone(), arg_id, arg_location, specialization_key)
    }

    pub fn get_for_this_before_method(
        method_id: &MethodIdentifier,
        method_location: Option<HPos>,
        pos: Option<HPos>,
    ) -> Self {
        let label = format!("$this in {} before {}", method_id.0, method_id.1);

        let mut specialization_key = None;

        if let Some(pos) = pos {
            specialization_key = Some(format!("{}:{}", pos.file_path, pos.start_offset));
        }

        DataFlowNode::new(label.clone(), label, method_location, specialization_key)
    }

    pub fn get_for_this_after_method(
        method_id: &MethodIdentifier,
        method_location: Option<HPos>,
        pos: Option<HPos>,
    ) -> Self {
        let label = format!("$this in {} after {}", method_id.0, method_id.1);

        let mut specialization_key = None;

        if let Some(pos) = pos {
            specialization_key = Some(format!("{}:{}", pos.file_path, pos.start_offset));
        }

        DataFlowNode::new(label.clone(), label, method_location, specialization_key)
    }

    pub fn get_for_assignment(var_id: String, assignment_location: HPos) -> Self {
        let id = format!(
            "{}-{}:{}-{}",
            var_id,
            assignment_location.file_path,
            assignment_location.start_offset,
            assignment_location.end_offset
        );

        Self::new(id, var_id, Some(assignment_location), None)
    }

    pub fn get_for_composition(assignment_location: HPos) -> Self {
        let id = format!(
            "composition-{}:{}-{}",
            assignment_location.file_path,
            assignment_location.start_offset,
            assignment_location.end_offset
        );

        Self::new(id.clone(), id, Some(assignment_location), None)
    }

    pub fn get_for_variable_sink(label: String, assignment_location: HPos) -> Self {
        let id = format!(
            "{}-{}:{}-{}",
            label,
            assignment_location.file_path,
            assignment_location.start_offset,
            assignment_location.end_offset
        );

        Self::VariableUseSink {
            id,
            pos: assignment_location,
        }
    }

    pub fn get_for_variable_source(label: String, assignment_location: HPos) -> Self {
        let id = format!(
            "{}-{}:{}-{}",
            label,
            assignment_location.file_path,
            assignment_location.start_offset,
            assignment_location.end_offset
        );

        Self::VariableUseSource {
            kind: VariableSourceKind::Default,
            id,
            pos: assignment_location,
        }
    }

    pub fn get_for_method_return(
        method_id: String,
        pos: Option<HPos>,
        function_location: Option<HPos>,
    ) -> Self {
        let mut specialization_key = None;

        if let Some(function_location) = function_location {
            specialization_key = Some(
                (*function_location.file_path).clone()
                    + ":"
                    + function_location.start_offset.to_string().as_str(),
            );
        }

        Self::new(
            method_id.clone(),
            format!("{}()", method_id),
            pos,
            specialization_key,
        )
    }

    pub fn get_for_method_reference(method_id: String, pos: HPos) -> Self {
        Self::new(
            format!("fnref-{}", method_id),
            format!("{}()", method_id),
            Some(pos),
            None,
        )
    }

    #[inline]
    pub fn get_id(&self) -> &String {
        match self {
            DataFlowNode::Vertex { id, .. }
            | DataFlowNode::TaintSource { id, .. }
            | DataFlowNode::TaintSink { id, .. }
            | DataFlowNode::VariableUseSource { id, .. }
            | DataFlowNode::VariableUseSink { id, .. } => id,
        }
    }

    #[inline]
    pub fn get_label(&self) -> &String {
        match self {
            DataFlowNode::Vertex { label, .. }
            | DataFlowNode::TaintSource { label, .. }
            | DataFlowNode::TaintSink { label, .. }
            | DataFlowNode::VariableUseSource { id: label, .. }
            | DataFlowNode::VariableUseSink { id: label, .. } => label,
        }
    }

    #[inline]
    pub fn get_pos(&self) -> &Option<HPos> {
        match self {
            DataFlowNode::Vertex { pos, .. }
            | DataFlowNode::TaintSource { pos, .. }
            | DataFlowNode::TaintSink { pos, .. } => pos,
            DataFlowNode::VariableUseSource { .. } | DataFlowNode::VariableUseSink { .. } => {
                panic!()
            }
        }
    }
}
