use bstr::BString;
use hakana_reflection_info::classlike_info::ClassConstantType;
use hakana_reflection_info::code_location::HPos;
use hakana_reflection_info::codebase_info::CodebaseInfo;
use hakana_reflection_info::data_flow::graph::{DataFlowGraph, GraphKind};
use hakana_reflection_info::data_flow::node::{DataFlowNode, DataFlowNodeKind};
use hakana_reflection_info::data_flow::path::{ArrayDataKind, PathKind};
use hakana_reflection_info::function_context::FunctionLikeIdentifier;
use hakana_reflection_info::functionlike_info::FunctionLikeInfo;
use hakana_reflection_info::t_atomic::{DictKey, TAtomic};
use hakana_reflection_info::t_union::TUnion;
use hakana_reflection_info::taint::SinkType;
use hakana_reflection_info::GenericParent;
use hakana_str::{Interner, StrId};
use hakana_type::type_comparator::type_comparison_result::TypeComparisonResult;
use hakana_type::type_comparator::union_type_comparator;
use hakana_type::type_expander::TypeExpansionOptions;
use hakana_type::{
    add_union_type, get_arrayish_params, get_float, get_int, get_literal_string, get_mixed,
    get_mixed_any, get_mixed_vec, get_nothing, get_null, get_object, get_string, get_vec, template,
    type_expander, wrap_atomic,
};
use rustc_hash::FxHashMap;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::expr::binop::concat_analyzer::analyze_concat_nodes;
use crate::expr::fetch::array_fetch_analyzer::handle_array_access_on_dict;
use crate::expr::variable_fetch_analyzer;
use crate::function_analysis_data::FunctionAnalysisData;
use crate::scope_analyzer::ScopeAnalyzer;
use crate::scope_context::ScopeContext;
use crate::statements_analyzer::StatementsAnalyzer;

use hakana_type::template::{TemplateBound, TemplateResult};
use oxidized::pos::Pos;
use oxidized::{aast, ast_defs};

pub(crate) fn fetch(
    statements_analyzer: &StatementsAnalyzer,
    expr: (
        (&Pos, &ast_defs::Id_),
        &Vec<aast::Targ<()>>,
        &Vec<(ast_defs::ParamKind, aast::Expr<(), ()>)>,
        &Option<aast::Expr<(), ()>>,
    ),
    pos: &Pos,
    functionlike_id: &FunctionLikeIdentifier,
    function_storage: &FunctionLikeInfo,
    mut template_result: TemplateResult,
    analysis_data: &mut FunctionAnalysisData,
    context: &mut ScopeContext,
) -> TUnion {
    let codebase = statements_analyzer.get_codebase();
    let mut stmt_type = None;

    if let FunctionLikeIdentifier::Function(name) = functionlike_id {
        if let Some(t) = handle_special_functions(
            statements_analyzer,
            name,
            expr.2,
            pos,
            codebase,
            analysis_data,
            context,
        ) {
            stmt_type = Some(t);
        }
    }

    // todo support custom return type providers for functions

    let stmt_type = if let Some(stmt_type) = stmt_type {
        stmt_type
    } else if let Some(function_return_type) = &function_storage.return_type {
        if !function_storage.template_types.is_empty()
            && !function_storage.template_types.is_empty()
        {
            let fn_id = match functionlike_id {
                FunctionLikeIdentifier::Function(function_id) => function_id,
                FunctionLikeIdentifier::Method(_, _) => panic!(),
                _ => {
                    panic!()
                }
            };

            for (template_name, _) in &function_storage.template_types {
                if template_result.lower_bounds.get(template_name).is_none() {
                    template_result.lower_bounds.insert(
                        *template_name,
                        FxHashMap::from_iter([(
                            GenericParent::FunctionLike(*fn_id),
                            vec![TemplateBound::new(get_nothing(), 1, None, None)],
                        )]),
                    );
                }
            }
        }

        let mut function_return_type = function_return_type.clone();

        if !template_result.lower_bounds.is_empty() && !function_storage.template_types.is_empty() {
            type_expander::expand_union(
                codebase,
                &Some(statements_analyzer.get_interner()),
                &mut function_return_type,
                &TypeExpansionOptions {
                    expand_templates: false,
                    ..Default::default()
                },
                &mut analysis_data.data_flow_graph,
            );

            function_return_type = template::inferred_type_replacer::replace(
                &function_return_type,
                &template_result,
                codebase,
            );
        }

        type_expander::expand_union(
            codebase,
            &Some(statements_analyzer.get_interner()),
            &mut function_return_type,
            &TypeExpansionOptions {
                expand_templates: false,
                expand_generic: true,
                file_path: Some(
                    &statements_analyzer
                        .get_file_analyzer()
                        .get_file_source()
                        .file_path,
                ),
                ..Default::default()
            },
            &mut analysis_data.data_flow_graph,
        );

        // todo dispatch AfterFunctionCallAnalysisEvent

        function_return_type
    } else {
        get_mixed_any()
    };

    add_dataflow(
        statements_analyzer,
        expr,
        pos,
        functionlike_id,
        function_storage,
        stmt_type,
        &template_result,
        analysis_data,
        context,
    )
}

fn handle_special_functions(
    statements_analyzer: &StatementsAnalyzer,
    name: &StrId,
    args: &Vec<(ast_defs::ParamKind, aast::Expr<(), ()>)>,
    pos: &Pos,
    codebase: &CodebaseInfo,
    analysis_data: &mut FunctionAnalysisData,
    context: &mut ScopeContext,
) -> Option<TUnion> {
    match name {
        &StrId::TYPE_STRUCTURE_FN => {
            if let (Some((_, first_arg_expr)), Some((_, second_arg_expr))) =
                (args.first(), args.get(1))
            {
                if let (Some(first_expr_type), Some(second_expr_type)) = (
                    analysis_data.get_expr_type(first_arg_expr.pos()),
                    analysis_data.get_expr_type(second_arg_expr.pos()),
                ) {
                    get_type_structure_type(
                        statements_analyzer,
                        first_expr_type,
                        second_expr_type,
                        context.function_context.calling_class,
                    )
                } else {
                    None
                }
            } else {
                None
            }
        }
        &StrId::GLOBAL_GET => {
            if let Some((_, arg_expr)) = args.first() {
                if let Some(expr_type) = analysis_data.get_expr_type(arg_expr.pos()) {
                    expr_type.get_single_literal_string_value().map(|value| {
                        variable_fetch_analyzer::get_type_for_superglobal(
                            statements_analyzer,
                            value,
                            pos,
                            analysis_data,
                        )
                    })
                } else {
                    None
                }
            } else {
                None
            }
        }
        &StrId::PREG_SPLIT => {
            if let Some((_, arg_expr)) = args.get(3) {
                if let Some(expr_type) = analysis_data.get_expr_type(arg_expr.pos()) {
                    return if let Some(value) = expr_type.get_single_literal_int_value() {
                        match value {
                            0 | 2 => {
                                let mut false_or_string_vec = TUnion::new(vec![
                                    TAtomic::TVec {
                                        known_items: None,
                                        type_param: Box::new(get_string()),
                                        known_count: None,
                                        non_empty: true,
                                    },
                                    TAtomic::TFalse,
                                ]);
                                false_or_string_vec.ignore_falsable_issues = true;
                                Some(false_or_string_vec)
                            }
                            1 | 3 => {
                                let mut false_or_string_vec = TUnion::new(vec![
                                    TAtomic::TVec {
                                        known_items: None,
                                        type_param: Box::new(get_string()),
                                        known_count: None,
                                        non_empty: false,
                                    },
                                    TAtomic::TFalse,
                                ]);
                                false_or_string_vec.ignore_falsable_issues = true;
                                Some(false_or_string_vec)
                            }
                            _ => {
                                let mut false_or_string_vec = TUnion::new(vec![
                                    TAtomic::TVec {
                                        known_items: None,
                                        type_param: Box::new(wrap_atomic(TAtomic::TVec {
                                            known_items: Some(BTreeMap::from([
                                                (0, (false, get_string())),
                                                (1, (false, get_int())),
                                            ])),
                                            type_param: Box::new(get_nothing()),
                                            known_count: None,
                                            non_empty: true,
                                        })),
                                        known_count: None,
                                        non_empty: false,
                                    },
                                    TAtomic::TFalse,
                                ]);
                                false_or_string_vec.ignore_falsable_issues = true;
                                Some(false_or_string_vec)
                            }
                        }
                    } else {
                        let mut false_or_string_vec = TUnion::new(vec![
                            TAtomic::TVec {
                                known_items: None,
                                type_param: Box::new(get_mixed()),
                                known_count: None,
                                non_empty: true,
                            },
                            TAtomic::TFalse,
                        ]);
                        false_or_string_vec.ignore_falsable_issues = true;
                        Some(false_or_string_vec)
                    };
                }
            } else {
                let mut false_or_string_vec = TUnion::new(vec![
                    TAtomic::TVec {
                        known_items: None,
                        type_param: Box::new(get_string()),
                        known_count: None,
                        non_empty: true,
                    },
                    TAtomic::TFalse,
                ]);
                false_or_string_vec.ignore_falsable_issues = true;
                return Some(false_or_string_vec);
            }

            None
        }
        &StrId::DEBUG_BACKTRACE => Some(wrap_atomic(TAtomic::TVec {
            known_items: None,
            type_param: Box::new(wrap_atomic(TAtomic::TDict {
                known_items: Some(BTreeMap::from([
                    (
                        DictKey::String("file".to_string()),
                        (false, Arc::new(get_string())),
                    ),
                    (
                        DictKey::String("line".to_string()),
                        (false, Arc::new(get_int())),
                    ),
                    (
                        DictKey::String("function".to_string()),
                        (false, Arc::new(get_string())),
                    ),
                    (
                        DictKey::String("class".to_string()),
                        (true, Arc::new(get_string())),
                    ),
                    (
                        DictKey::String("object".to_string()),
                        (true, Arc::new(get_object())),
                    ),
                    (
                        DictKey::String("type".to_string()),
                        (true, Arc::new(get_string())),
                    ),
                    (
                        DictKey::String("args".to_string()),
                        (true, Arc::new(get_mixed_vec())),
                    ),
                ])),
                params: None,
                non_empty: true,
                shape_name: None,
            })),
            known_count: None,
            non_empty: true,
        })),
        &StrId::STR_REPLACE => {
            // returns string if the second arg is a string
            if let Some((_, arg_expr)) = args.get(1) {
                if let Some(expr_type) = analysis_data.get_expr_type(arg_expr.pos()) {
                    if union_type_comparator::is_contained_by(
                        codebase,
                        expr_type,
                        &get_string(),
                        false,
                        expr_type.ignore_falsable_issues,
                        false,
                        &mut TypeComparisonResult::new(),
                    ) {
                        Some(get_string())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
        &StrId::PREG_REPLACE => {
            // returns string if the third arg is a string
            if let Some((_, arg_expr)) = args.get(2) {
                if let Some(expr_type) = analysis_data.get_expr_type(arg_expr.pos()) {
                    if union_type_comparator::is_contained_by(
                        codebase,
                        expr_type,
                        &get_string(),
                        false,
                        expr_type.ignore_falsable_issues,
                        false,
                        &mut TypeComparisonResult::new(),
                    ) {
                        let null_or_string = TUnion::new(vec![TAtomic::TString, TAtomic::TNull]);
                        Some(null_or_string)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
        &StrId::MICROTIME => {
            if let Some((_, arg_expr)) = args.first() {
                if let Some(expr_type) = analysis_data.get_expr_type(arg_expr.pos()) {
                    if expr_type.is_always_truthy() {
                        Some(get_float())
                    } else if expr_type.is_always_falsy() {
                        Some(get_string())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
        &StrId::LIB_STR_JOIN => {
            if let (Some((_, first_arg_expr)), Some((_, second_arg_expr))) =
                (args.first(), args.get(1))
            {
                if let (Some(first_expr_type), Some(second_expr_type)) = (
                    analysis_data.get_expr_type(first_arg_expr.pos()),
                    analysis_data.get_expr_type(second_arg_expr.pos()),
                ) {
                    if second_expr_type.all_literals() && first_expr_type.is_single() {
                        let first_expr_type = first_expr_type.get_single();
                        let first_arg_params = get_arrayish_params(first_expr_type, codebase);

                        if let Some(first_arg_params) = first_arg_params {
                            if first_arg_params.1.all_literals() {
                                Some(wrap_atomic(TAtomic::TStringWithFlags(true, false, true)))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
        &StrId::LIB_STR_FORMAT => {
            if let Some(first_arg) = args.first() {
                if let aast::Expr_::String(simple_string) = &first_arg.1 .2 {
                    let mut escaped = false;
                    let mut in_format_string = false;

                    let mut literals = vec![];

                    let mut cur_literal = "".to_string();

                    for c in simple_string.iter().copied() {
                        if in_format_string {
                            in_format_string = false;
                            continue;
                        }

                        if !escaped {
                            if c as char == '%' {
                                in_format_string = true;
                                literals.push(aast::Expr(
                                    (),
                                    first_arg.1.pos().clone(),
                                    aast::Expr_::String(BString::from(cur_literal)),
                                ));
                                cur_literal = "".to_string();
                                continue;
                            }

                            if c as char == '\\' {
                                escaped = true;
                            }

                            in_format_string = false;
                        } else {
                            if c as char == '\\' {
                                cur_literal += "\\";
                                escaped = false;
                                continue;
                            }

                            escaped = false;
                        }

                        cur_literal += (c as char).to_string().as_str();
                    }

                    literals.push(aast::Expr(
                        (),
                        first_arg.1.pos().clone(),
                        aast::Expr_::String(BString::from(cur_literal)),
                    ));

                    let mut concat_args = vec![];

                    for (i, literal) in literals.iter().enumerate() {
                        concat_args.push(literal);
                        if let Some(arg) = args.get(i + 1) {
                            concat_args.push(&arg.1);
                        } else {
                            break;
                        }
                    }

                    let result_type =
                        analyze_concat_nodes(concat_args, statements_analyzer, analysis_data, pos);

                    return Some(result_type);
                }
            }

            None
        }
        &StrId::LIB_STR_TRIM
        | &StrId::LIB_STR_STRIP_SUFFIX
        | &StrId::LIB_STR_SLICE
        | &StrId::LIB_STR_REPLACE => {
            let mut all_literals = true;
            for (_, arg_expr) in args {
                if let Some(arg_expr_type) = analysis_data.get_expr_type(arg_expr.pos()) {
                    if !arg_expr_type.all_literals() {
                        all_literals = false;
                        break;
                    }
                } else {
                    all_literals = false;
                    break;
                }
            }

            Some(wrap_atomic(if all_literals {
                TAtomic::TStringWithFlags(false, false, true)
            } else {
                TAtomic::TString
            }))
        }
        &StrId::LIB_STR_SPLIT => {
            let mut all_literals = true;
            for (_, arg_expr) in args {
                if let Some(arg_expr_type) = analysis_data.get_expr_type(arg_expr.pos()) {
                    if !arg_expr_type.all_literals() {
                        all_literals = false;
                        break;
                    }
                } else {
                    all_literals = false;
                    break;
                }
            }

            Some(get_vec(wrap_atomic(if all_literals {
                TAtomic::TStringWithFlags(false, false, true)
            } else {
                TAtomic::TString
            })))
        }
        &StrId::RANGE => {
            let mut all_ints = true;
            for (_, arg_expr) in args {
                if let Some(arg_expr_type) = analysis_data.get_expr_type(arg_expr.pos()) {
                    if !arg_expr_type.is_int() {
                        all_ints = false;
                        break;
                    }
                } else {
                    all_ints = false;
                    break;
                }
            }

            if all_ints {
                Some(get_vec(get_int()))
            } else {
                None
            }
        }
        &StrId::IDX_FN => {
            if args.len() >= 2 {
                let dict_type = analysis_data.get_rc_expr_type(args[0].1.pos()).cloned();
                let dim_type = analysis_data.get_rc_expr_type(args[1].1.pos()).cloned();

                let mut expr_type = None;

                if let (Some(dict_type), Some(dim_type)) = (dict_type, dim_type) {
                    for atomic_type in &dict_type.types {
                        if let TAtomic::TDict { .. } = atomic_type {
                            let mut expr_type_inner = handle_array_access_on_dict(
                                statements_analyzer,
                                pos,
                                analysis_data,
                                context,
                                atomic_type,
                                &dim_type,
                                false,
                                &mut false,
                                true,
                                &mut false,
                                &mut false,
                            );

                            if args.len() == 2 && !expr_type_inner.is_mixed() {
                                expr_type_inner =
                                    add_union_type(expr_type_inner, &get_null(), codebase, false);
                            }

                            expr_type = Some(expr_type_inner);
                        }
                    }

                    if args.len() > 2 {
                        let default_type = analysis_data.get_expr_type(args[2].1.pos());
                        expr_type = expr_type.map(|expr_type| {
                            if let Some(default_type) = default_type {
                                add_union_type(expr_type, default_type, codebase, false)
                            } else {
                                add_union_type(expr_type, &get_mixed_any(), codebase, false)
                            }
                        });
                    }
                }

                Some(expr_type.unwrap_or(get_mixed_any()))
            } else {
                None
            }
        }
        &StrId::DIRNAME => {
            if args.len() == 1 {
                let file_type = analysis_data.get_rc_expr_type(args[0].1.pos()).cloned();

                if let Some(file_type) = file_type {
                    if let Some(literal_value) = file_type.get_single_literal_string_value() {
                        let path = Path::new(&literal_value);
                        if let Some(dir) = path.parent() {
                            return Some(get_literal_string(dir.to_str().unwrap().to_owned()));
                        }
                    }
                }
            }

            None
        }
        _ => None,
    }
}

fn get_type_structure_type(
    statements_analyzer: &StatementsAnalyzer,
    first_expr_type: &TUnion,
    second_expr_type: &TUnion,
    this_class: Option<StrId>,
) -> Option<TUnion> {
    if let Some(second_arg_string) = second_expr_type.get_single_literal_string_value() {
        let const_name = statements_analyzer.get_interner().get(&second_arg_string)?;

        if first_expr_type.is_single() {
            let classname = match first_expr_type.get_single() {
                TAtomic::TLiteralClassname { name } => *name,
                TAtomic::TClassname { as_type } => match &**as_type {
                    TAtomic::TNamedObject { name, is_this, .. } => {
                        if *is_this {
                            if let Some(this_class) = this_class {
                                this_class
                            } else {
                                *name
                            }
                        } else {
                            *name
                        }
                    }
                    _ => {
                        return None;
                    }
                },
                TAtomic::TNamedObject { name, is_this, .. } => {
                    if *is_this {
                        if let Some(this_class) = this_class {
                            this_class
                        } else {
                            *name
                        }
                    } else {
                        *name
                    }
                }
                _ => {
                    return None;
                }
            };

            if let Some(classlike_info) = statements_analyzer
                .get_codebase()
                .classlike_infos
                .get(&classname)
            {
                if let Some(type_constant_info) = classlike_info.type_constants.get(&const_name) {
                    return Some(wrap_atomic(TAtomic::TTypeAlias {
                        name: StrId::TYPE_STRUCTURE,
                        type_params: Some(vec![match type_constant_info {
                            ClassConstantType::Concrete(actual_type) => actual_type.clone(),
                            ClassConstantType::Abstract(Some(as_type)) => as_type.clone(),
                            _ => get_mixed_any(),
                        }]),
                        as_type: None,
                    }));
                }
            }
        }
    }

    None
}

fn add_dataflow(
    statements_analyzer: &StatementsAnalyzer,
    expr: (
        (&Pos, &ast_defs::Id_),
        &Vec<aast::Targ<()>>,
        &Vec<(ast_defs::ParamKind, aast::Expr<(), ()>)>,
        &Option<aast::Expr<(), ()>>,
    ),
    pos: &Pos,
    functionlike_id: &FunctionLikeIdentifier,
    functionlike_storage: &FunctionLikeInfo,
    stmt_type: TUnion,
    _template_result: &TemplateResult,
    analysis_data: &mut FunctionAnalysisData,
    context: &mut ScopeContext,
) -> TUnion {
    // todo dispatch AddRemoveTaintsEvent

    //let added_taints = Vec::new();
    //let removed_taints = Vec::new();

    let data_flow_graph = &mut analysis_data.data_flow_graph;

    if let GraphKind::WholeProgram(_) = &data_flow_graph.kind {
        if !context.allow_taints {
            return stmt_type;
        }
    }

    let mut stmt_type = stmt_type;

    // todo conditionally remove taints

    let function_call_node = if let GraphKind::WholeProgram(_) = &data_flow_graph.kind {
        DataFlowNode::get_for_method_return(
            functionlike_id.to_string(statements_analyzer.get_interner()),
            if let Some(return_pos) = &functionlike_storage.return_type_location {
                Some(*return_pos)
            } else {
                functionlike_storage.name_location
            },
            if functionlike_storage.specialize_call {
                Some(statements_analyzer.get_hpos(pos))
            } else {
                None
            },
        )
    } else {
        DataFlowNode::get_for_method_return(
            functionlike_id.to_string(statements_analyzer.get_interner()),
            Some(statements_analyzer.get_hpos(pos)),
            Some(statements_analyzer.get_hpos(pos)),
        )
    };

    data_flow_graph.add_node(function_call_node.clone());

    let (param_offsets, variadic_path) = get_special_argument_nodes(functionlike_id, expr);

    let added_removed_taints = if let GraphKind::WholeProgram(_) = &data_flow_graph.kind {
        get_special_added_removed_taints(functionlike_id, statements_analyzer.get_interner())
    } else {
        FxHashMap::default()
    };

    let mut last_arg = usize::MAX;

    for (param_offset, path_kind) in param_offsets {
        if let Some(arg) = expr.2.get(param_offset) {
            let arg_pos = statements_analyzer.get_hpos(arg.1.pos());

            add_special_param_dataflow(
                statements_analyzer,
                functionlike_id,
                true,
                param_offset,
                arg_pos,
                pos,
                &added_removed_taints,
                data_flow_graph,
                &function_call_node,
                path_kind,
            );
        }

        last_arg = param_offset;
    }

    if let Some(path_kind) = &variadic_path {
        for (param_offset, (_, arg)) in expr.2.iter().enumerate() {
            if last_arg == usize::MAX || param_offset > last_arg {
                let arg_pos = statements_analyzer.get_hpos(arg.pos());

                add_special_param_dataflow(
                    statements_analyzer,
                    functionlike_id,
                    true,
                    param_offset,
                    arg_pos,
                    pos,
                    &added_removed_taints,
                    data_flow_graph,
                    &function_call_node,
                    path_kind.clone(),
                );
            }
        }

        if let Some(expanded_arg) = expr.3 {
            add_special_param_dataflow(
                statements_analyzer,
                functionlike_id,
                true,
                expr.2.len(),
                statements_analyzer.get_hpos(expanded_arg.pos()),
                pos,
                &added_removed_taints,
                data_flow_graph,
                &function_call_node,
                path_kind.clone(),
            );
        }
    }

    if let GraphKind::WholeProgram(_) = &data_flow_graph.kind {
        if !functionlike_storage.taint_source_types.is_empty() {
            let function_call_node_source = DataFlowNode {
                id: function_call_node.get_id().clone(),
                kind: DataFlowNodeKind::TaintSource {
                    pos: *function_call_node.get_pos(),
                    label: function_call_node.get_label().clone(),
                    types: functionlike_storage.taint_source_types.clone(),
                },
            };
            data_flow_graph.add_node(function_call_node_source);
        }
    }

    stmt_type.parent_nodes.push(function_call_node);

    stmt_type
}

pub(crate) fn add_special_param_dataflow(
    statements_analyzer: &StatementsAnalyzer,
    functionlike_id: &FunctionLikeIdentifier,
    specialize_call: bool,
    param_offset: usize,
    arg_pos: HPos,
    pos: &Pos,
    added_removed_taints: &FxHashMap<usize, (Vec<SinkType>, Vec<SinkType>)>,
    data_flow_graph: &mut DataFlowGraph,
    function_call_node: &DataFlowNode,
    path_kind: PathKind,
) {
    let argument_node = DataFlowNode::get_for_method_argument(
        functionlike_id.to_string(statements_analyzer.get_interner()),
        param_offset,
        Some(arg_pos),
        if specialize_call {
            Some(statements_analyzer.get_hpos(pos))
        } else {
            None
        },
    );

    let (added_taints, removed_taints) =
        if let Some(added_removed_taints) = added_removed_taints.get(&param_offset) {
            added_removed_taints.clone()
        } else {
            (vec![], vec![])
        };

    data_flow_graph.add_path(
        &argument_node,
        function_call_node,
        path_kind,
        added_taints,
        removed_taints,
    );
    data_flow_graph.add_node(argument_node);
}

/*
Returns a list of paths with (input_argument_position, path to return output).
The optional path is for functions with ... params.
*/
fn get_special_argument_nodes(
    functionlike_id: &FunctionLikeIdentifier,
    expr: (
        (&Pos, &ast_defs::Id_),
        &Vec<aast::Targ<()>>,
        &Vec<(ast_defs::ParamKind, aast::Expr<(), ()>)>,
        &Option<aast::Expr<(), ()>>,
    ),
) -> (Vec<(usize, PathKind)>, Option<PathKind>) {
    match functionlike_id {
        FunctionLikeIdentifier::Function(function_name) => match *function_name {
            StrId::VAR_EXPORT
            | StrId::PRINT_R
            | StrId::HIGHLIGHT_STRING
            | StrId::STRTOLOWER
            | StrId::STRTOUPPER
            | StrId::TRIM
            | StrId::LTRIM
            | StrId::RTRIM
            | StrId::LIB_STR_TRIM
            | StrId::LIB_STR_TRIM_LEFT
            | StrId::LIB_STR_TRIM_RIGHT
            | StrId::LIB_STR_LOWERCASE
            | StrId::LIB_STR_UPPERCASE
            | StrId::LIB_STR_CAPITALIZE
            | StrId::LIB_STR_CAPITALIZE_WORDS
            | StrId::ASIO_JOIN
            | StrId::STRIP_TAGS
            | StrId::STRIPSLASHES
            | StrId::STRIPCSLASHES
            | StrId::HTMLENTITIES
            | StrId::HTMLENTITYDECODE
            | StrId::HTMLSPECIALCHARS
            | StrId::HTMLSPECIALCHARS_DECODE
            | StrId::STR_REPEAT
            | StrId::STR_ROT13
            | StrId::STR_SHUFFLE
            | StrId::STRSTR
            | StrId::STRISTR
            | StrId::STRCHR
            | StrId::STRPBRK
            | StrId::STRRCHR
            | StrId::STRREV
            | StrId::PREG_QUOTE
            | StrId::WORDWRAP
            | StrId::REALPATH
            | StrId::STRVAL
            | StrId::STRGETCSV
            | StrId::ADDCSLASHES
            | StrId::ADDSLASHES
            | StrId::UCFIRST
            | StrId::UCWORDS
            | StrId::LCFIRST
            | StrId::NL2BR
            | StrId::QUOTED_PRINTABLE_DECODE
            | StrId::QUOTED_PRINTABLE_ENCODE
            | StrId::QUOTE_META
            | StrId::CHOP
            | StrId::CONVERT_UUDECODE
            | StrId::CONVERT_UUENCODE
            | StrId::JSON_DECODE
            | StrId::BASE64_ENCODE
            | StrId::BASE64_DECODE
            | StrId::URLENCODE
            | StrId::URLDECODE
            | StrId::GZINFLATE
            | StrId::LIB_DICT_FILTER
            | StrId::LIB_DICT_FILTER_ASYNC
            | StrId::LIB_DICT_FILTER_KEYS
            | StrId::LIB_DICT_FILTER_NULLS
            | StrId::LIB_DICT_FILTER_WITH_KEY
            | StrId::LIB_DICT_FLATTEN
            | StrId::LIB_VEC_FILTER
            | StrId::LIB_VEC_FILTER_ASYNC
            | StrId::LIB_VEC_FILTER_NULLS
            | StrId::LIB_VEC_FILTER_WITH_KEY
            | StrId::LIB_VEC_DROP
            | StrId::LIB_VEC_REVERSE
            | StrId::LIB_DICT_REVERSE
            | StrId::LIB_VEC_UNIQUE
            | StrId::LIB_KEYSET_FILTER
            | StrId::LIB_KEYSET_FILTER_NULLS
            | StrId::LIB_KEYSET_FILTER_ASYNC
            | StrId::LIB_KEYSET_FLATTEN
            | StrId::LIB_KEYSET_KEYS
            | StrId::KEYSET
            | StrId::VEC
            | StrId::DICT
            | StrId::GET_OBJECT_VARS
            | StrId::RAWURLENCODE
            | StrId::LIB_DICT_FROM_ASYNC
            | StrId::LIB_VEC_FROM_ASYNC
            | StrId::ORD
            | StrId::LOG
            | StrId::IP2LONG
            | StrId::BIN2HEX
            | StrId::HEX2BIN
            | StrId::ESCAPESHELLARG => (vec![(0, PathKind::Default)], None),
            StrId::LIB_REGEX_FIRST_MATCH => (vec![(0, PathKind::Default)], Some(PathKind::Default)),
            StrId::LIB_DICT_SELECT_KEYS
            | StrId::LIB_VEC_TAKE
            | StrId::LIB_DICT_TAKE
            | StrId::LIB_STR_SLICE
            | StrId::LIB_STR_FORMAT_NUMBER
            | StrId::LIB_DICT_DIFF_BY_KEY
            | StrId::NUMBER_FORMAT
            | StrId::LIB_DICT_CHUNK
            | StrId::LIB_VEC_DIFF
            | StrId::LIB_KEYSET_DIFF
            | StrId::LIB_KEYSET_INTERSECT
            | StrId::LIB_VEC_INTERSECT
            | StrId::LIB_VEC_SLICE
            | StrId::LIB_VEC_RANGE
            | StrId::LIB_VEC_CHUNK
            | StrId::LIB_KEYSET_CHUNK
            | StrId::LIB_STR_STRIP_PREFIX
            | StrId::LIB_STR_STRIP_SUFFIX
            | StrId::LIB_STR_REPEAT
            | StrId::SUBSTR
            | StrId::LIB_DICT_ASSOCIATE => {
                (vec![(0, PathKind::Default)], Some(PathKind::Aggregate))
            }
            StrId::LIB_C_IS_EMPTY
            | StrId::LIB_C_COUNT
            | StrId::COUNT
            | StrId::LIB_C_ANY
            | StrId::LIB_C_EVERY
            | StrId::LIB_C_SEARCH
            | StrId::LIB_STR_IS_EMPTY
            | StrId::LIB_STR_LENGTH
            | StrId::LIB_VEC_KEYS
            | StrId::LIB_STR_TO_INT
            | StrId::LIB_MATH_ROUND
            | StrId::LIB_MATH_SUM
            | StrId::LIB_MATH_SUM_FLOAT
            | StrId::LIB_MATH_MIN
            | StrId::LIB_MATH_MIN_BY
            | StrId::LIB_MATH_MAX
            | StrId::LIB_MATH_MEAN
            | StrId::LIB_MATH_MEDIAN
            | StrId::LIB_MATH_CEIL
            | StrId::LIB_MATH_COS
            | StrId::LIB_MATH_FLOOR
            | StrId::LIB_MATH_IS_NAN
            | StrId::LIB_MATH_LOG
            | StrId::LIB_MATH_SIN
            | StrId::LIB_MATH_SQRT
            | StrId::LIB_MATH_TAN
            | StrId::LIB_MATH_ABS
            | StrId::INTVAL
            | StrId::GET_CLASS
            | StrId::CTYPE_LOWER
            | StrId::SHA1
            | StrId::MD5
            | StrId::DIRNAME
            | StrId::CRC32
            | StrId::FILTER_VAR => (vec![(0, PathKind::Aggregate)], None),
            StrId::LIB_MATH_ALMOST_EQUALS
            | StrId::LIB_MATH_BASE_CONVERT
            | StrId::LIB_MATH_EXP
            | StrId::LIB_MATH_FROM_BASE
            | StrId::LIB_MATH_INT_DIV
            | StrId::LIB_MATH_TO_BASE
            | StrId::LIB_MATH_MAX_BY
            | StrId::LIB_MATH_MAXVA
            | StrId::LIB_MATH_MINVA
            | StrId::LIB_STR_STARTS_WITH
            | StrId::LIB_STR_STARTS_WITH_CI
            | StrId::LIB_STR_ENDS_WITH
            | StrId::LIB_STR_ENDS_WITH_CI
            | StrId::LIB_STR_SEARCH
            | StrId::LIB_STR_CONTAINS
            | StrId::LIB_STR_CONTAINS_CI
            | StrId::LIB_STR_COMPARE
            | StrId::LIB_STR_COMPARE_CI
            | StrId::HASH_EQUALS
            | StrId::RANGE
            | StrId::STRPOS
            | StrId::SUBSTR_COUNT
            | StrId::STRCMP
            | StrId::STRNATCASECMP
            | StrId::LIB_KEYSET_EQUAL => (vec![], Some(PathKind::Aggregate)),
            StrId::LIB_C_CONTAINS
            | StrId::LIB_C_CONTAINS_KEY
            | StrId::IN_ARRAY
            | StrId::PREG_MATCH
            | StrId::LIB_REGEX_MATCHES
            | StrId::PREG_MATCH_WITH_MATCHES
            | StrId::PREG_MATCH_ALL_WITH_MATCHES
            | StrId::HASH => (
                vec![
                    (0, PathKind::Aggregate),
                    (1, PathKind::Aggregate),
                    (3, PathKind::Aggregate),
                    (4, PathKind::Aggregate),
                ],
                None,
            ),
            StrId::JSON_ENCODE | StrId::SERIALIZE => (vec![(0, PathKind::Serialize)], None),
            StrId::VAR_DUMP | StrId::PRINTF => {
                (vec![(0, PathKind::Serialize)], Some(PathKind::Serialize))
            }
            StrId::SSCANF | StrId::SUBSTR_REPLACE => {
                (vec![(0, PathKind::Default), (1, PathKind::Default)], None)
            }
            StrId::STR_REPLACE | StrId::STR_IREPLACE | StrId::PREG_FILTER | StrId::PREG_REPLACE => {
                (
                    vec![
                        (0, PathKind::Aggregate),
                        (1, PathKind::Default),
                        (2, PathKind::Default),
                    ],
                    None,
                )
            }
            StrId::PREG_REPLACE_WITH_COUNT => (
                vec![
                    (0, PathKind::Aggregate),
                    (1, PathKind::Default),
                    (2, PathKind::Default),
                    (0, PathKind::Aggregate),
                ],
                None,
            ),
            StrId::PREG_GREP => (vec![(0, PathKind::Aggregate), (1, PathKind::Default)], None),
            StrId::LIB_STR_REPLACE_EVERY => (
                vec![
                    (0, PathKind::Default),
                    (1, PathKind::UnknownArrayFetch(ArrayDataKind::ArrayValue)),
                ],
                None,
            ),

            StrId::STR_PAD
            | StrId::LIB_STR_PAD_LEFT
            | StrId::LIB_STR_PAD_RIGHT
            | StrId::CHUNK_SPLIT
            | StrId::LIB_REGEX_REPLACE
            | StrId::LIB_STR_REPLACE
            | StrId::LIB_STR_REPLACE_CI => (
                vec![
                    (0, PathKind::Default),
                    (1, PathKind::Aggregate),
                    (2, PathKind::Default),
                ],
                None,
            ),
            StrId::IMPLODE | StrId::JOIN => (
                vec![
                    (0, PathKind::Default),
                    (1, PathKind::UnknownArrayFetch(ArrayDataKind::ArrayValue)),
                ],
                None,
            ),
            StrId::LIB_DICT_FILL_KEYS => (
                vec![
                    (0, PathKind::Default),
                    (
                        1,
                        PathKind::UnknownArrayAssignment(ArrayDataKind::ArrayValue),
                    ),
                ],
                None,
            ),
            StrId::HTTP_BUILD_QUERY => (
                vec![(0, PathKind::UnknownArrayFetch(ArrayDataKind::ArrayValue))],
                None,
            ),
            StrId::EXPLODE | StrId::PREG_SPLIT => (
                vec![(
                    1,
                    PathKind::UnknownArrayAssignment(ArrayDataKind::ArrayValue),
                )],
                None,
            ),
            StrId::LIB_VEC_ZIP => (
                vec![
                    (
                        0,
                        PathKind::UnknownArrayAssignment(ArrayDataKind::ArrayValue),
                    ),
                    (
                        1,
                        PathKind::UnknownArrayAssignment(ArrayDataKind::ArrayValue),
                    ),
                ],
                None,
            ),
            StrId::PATHINFO => (
                vec![
                    (
                        0,
                        PathKind::ArrayAssignment(ArrayDataKind::ArrayValue, "dirname".to_string()),
                    ),
                    (
                        0,
                        PathKind::ArrayAssignment(
                            ArrayDataKind::ArrayValue,
                            "basename".to_string(),
                        ),
                    ),
                    (
                        0,
                        PathKind::ArrayAssignment(
                            ArrayDataKind::ArrayValue,
                            "extension".to_string(),
                        ),
                    ),
                    (
                        0,
                        PathKind::ArrayAssignment(
                            ArrayDataKind::ArrayValue,
                            "filename".to_string(),
                        ),
                    ),
                ],
                None,
            ),
            StrId::STR_SPLIT
            | StrId::LIB_STR_SPLIT
            | StrId::LIB_STR_CHUNK
            | StrId::LIB_REGEX_EVERY_MATCH => (
                vec![
                    (
                        0,
                        PathKind::UnknownArrayAssignment(ArrayDataKind::ArrayValue),
                    ),
                    (1, PathKind::Aggregate),
                    (2, PathKind::Aggregate),
                ],
                None,
            ),
            StrId::LIB_VEC_SORT => (vec![(0, PathKind::Default)], None),
            StrId::LIB_STR_JOIN => (
                vec![
                    (0, PathKind::UnknownArrayFetch(ArrayDataKind::ArrayValue)),
                    (1, PathKind::Default),
                ],
                None,
            ),
            StrId::LIB_VEC_MAP
            | StrId::LIB_DICT_MAP
            | StrId::LIB_KEYSET_MAP
            | StrId::LIB_VEC_MAP_ASYNC
            | StrId::LIB_DICT_MAP_ASYNC
            | StrId::LIB_KEYSET_MAP_ASYNC
            | StrId::LIB_VEC_MAP_WITH_KEY
            | StrId::LIB_DICT_MAP_WITH_KEY
            | StrId::LIB_KEYSET_MAP_WITH_KEY
            | StrId::LIB_DICT_MAP_WITH_KEY_ASYNC => (
                vec![(
                    1,
                    PathKind::UnknownArrayAssignment(ArrayDataKind::ArrayValue),
                )],
                None,
            ),
            StrId::LIB_DICT_FROM_ENTRIES => (
                // todo improve this
                vec![(0, PathKind::Default)],
                None,
            ),
            StrId::LIB_DICT_FLIP => (
                // todo improve this
                vec![(0, PathKind::Default)],
                None,
            ),
            StrId::LIB_DICT_FROM_KEYS | StrId::LIB_DICT_FROM_KEYS_ASYNC => (
                vec![(
                    1,
                    PathKind::UnknownArrayAssignment(ArrayDataKind::ArrayValue),
                )],
                None,
            ),
            StrId::LIB_C_FIRST
            | StrId::LIB_C_FIRSTX
            | StrId::LIB_C_LAST
            | StrId::LIB_C_LASTX
            | StrId::LIB_C_ONLYX
            | StrId::LIB_C_FIND
            | StrId::LIB_C_FINDX => (
                vec![(0, PathKind::UnknownArrayFetch(ArrayDataKind::ArrayValue))],
                None,
            ),
            StrId::LIB_VEC_FLATTEN => (
                vec![(0, PathKind::UnknownArrayFetch(ArrayDataKind::ArrayValue))],
                None,
            ),
            StrId::IDX_FN => {
                if let Some(second_arg) = expr.2.get(1) {
                    if let aast::Expr_::String(str) = &second_arg.1 .2 {
                        return (
                            vec![
                                (
                                    0,
                                    PathKind::ArrayFetch(
                                        ArrayDataKind::ArrayValue,
                                        str.to_string(),
                                    ),
                                ),
                                (1, PathKind::Aggregate),
                            ],
                            None,
                        );
                    }
                }
                (
                    vec![
                        (0, PathKind::UnknownArrayFetch(ArrayDataKind::ArrayValue)),
                        (1, PathKind::Aggregate),
                    ],
                    None,
                )
            }
            StrId::LIB_C_FIRST_KEY
            | StrId::LIB_C_FIRST_KEYX
            | StrId::LIB_C_LAST_KEY
            | StrId::LIB_C_LAST_KEYX
            | StrId::LIB_C_FIND_KEY => (
                vec![(0, PathKind::UnknownArrayFetch(ArrayDataKind::ArrayKey))],
                None,
            ),
            StrId::LIB_DICT_MERGE | StrId::LIB_VEC_CONCAT | StrId::LIB_KEYSET_UNION => {
                (vec![(0, PathKind::Default)], Some(PathKind::Default))
            }
            _ => {
                // if function_name.starts_with("HH\\Lib\\")
                //     && !function_name.starts_with("HH\\Lib\\Math\\")
                // {
                //     println!("no taints through {}", function_name);
                // }
                (vec![], None)
            }
        },
        _ => panic!(),
    }
}

fn get_special_added_removed_taints(
    functionlike_id: &FunctionLikeIdentifier,
    interner: &Interner,
) -> FxHashMap<usize, (Vec<SinkType>, Vec<SinkType>)> {
    match functionlike_id {
        FunctionLikeIdentifier::Function(function_name) => match interner.lookup(function_name) {
            "html_entity_decode" | "htmlspecialchars_decode" => {
                FxHashMap::from_iter([(0, (vec![SinkType::HtmlTag], vec![]))])
            }
            "htmlentities" | "htmlspecialchars" | "strip_tags" | "urlencode" => {
                FxHashMap::from_iter([(
                    0,
                    (vec![], vec![SinkType::HtmlTag, SinkType::HtmlAttributeUri]),
                )])
            }
            _ => FxHashMap::default(),
        },
        _ => panic!(),
    }
}
