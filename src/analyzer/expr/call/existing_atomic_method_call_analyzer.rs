use std::rc::Rc;

use function_context::method_identifier::MethodIdentifier;
use hakana_reflection_info::{
    assertion::Assertion,
    data_flow::{node::DataFlowNode, path::PathKind},
    t_atomic::TAtomic,
    t_union::TUnion,
};
use hakana_type::{add_union_type, get_mixed_any, template::TemplateResult};
use indexmap::IndexMap;
use oxidized::{
    aast,
    ast_defs::{self, Pos},
};
use rustc_hash::FxHashMap;

use crate::{
    expr::{
        call_analyzer::check_method_args, expression_identifier,
        fetch::array_fetch_analyzer::handle_array_access_on_dict,
    },
    scope_analyzer::ScopeAnalyzer,
    scope_context::ScopeContext,
    statements_analyzer::StatementsAnalyzer,
    typed_ast::TastInfo,
};

use super::{
    atomic_method_call_analyzer::AtomicMethodCallAnalysisResult, class_template_param_collector,
    method_call_return_type_fetcher,
};

pub(crate) fn analyze(
    statements_analyzer: &StatementsAnalyzer,
    mut classlike_name: String,
    method_name: &String,
    call_expr: (
        &Vec<aast::Targ<()>>,
        &Vec<(ast_defs::ParamKind, aast::Expr<(), ()>)>,
        &Option<aast::Expr<(), ()>>,
    ),
    lhs_type_part: &TAtomic,
    pos: &Pos,
    tast_info: &mut TastInfo,
    context: &mut ScopeContext,
    if_body_context: &mut Option<ScopeContext>,
    lhs_var_id: Option<&String>,
    lhs_var_pos: Option<&Pos>,
    result: &mut AtomicMethodCallAnalysisResult,
) -> TUnion {
    tast_info
        .symbol_references
        .add_reference_to_symbol(&context.function_context, classlike_name.clone());

    if classlike_name == "static" {
        classlike_name = context.function_context.calling_class.clone().unwrap();
    }

    let method_id = MethodIdentifier(classlike_name.clone(), method_name.clone());

    result.existent_method_ids.insert(method_id.to_string());

    let codebase = statements_analyzer.get_codebase();

    let declaring_method_id = codebase.get_declaring_method_id(&method_id);

    let classlike_storage = codebase.classlike_infos.get(&classlike_name).unwrap();

    tast_info.symbol_references.add_reference_to_class_member(
        &context.function_context,
        (
            declaring_method_id.0.clone(),
            format!("{}()", declaring_method_id.1),
        ),
    );

    if let Some(overridden_classlikes) = classlike_storage
        .overridden_method_ids
        .get(&declaring_method_id.1)
    {
        for overridden_classlike in overridden_classlikes {
            tast_info
                .symbol_references
                .add_reference_to_overridden_class_member(
                    &context.function_context,
                    (
                        overridden_classlike.clone(),
                        format!("{}()", declaring_method_id.1),
                    ),
                );
        }
    }

    let class_template_params = if classlike_name != "HH\\Vector" || method_name != "fromItems" {
        class_template_param_collector::collect(
            codebase,
            codebase
                .classlike_infos
                .get(&declaring_method_id.0)
                .unwrap(),
            classlike_storage,
            Some(lhs_type_part),
            lhs_var_id.unwrap_or(&"".to_string()) == "$this",
        )
    } else {
        None
    };

    if lhs_var_id.cloned().unwrap_or_default() == "$this" {
        // todo check for analysis inside traits, update class_template_params
    }

    let functionlike_storage = codebase.get_method(&declaring_method_id).unwrap();

    // todo support if_this_is_type template params

    let mut template_result = TemplateResult::new(
        functionlike_storage.template_types.clone(),
        class_template_params.unwrap_or(IndexMap::new()),
    );

    if !functionlike_storage.pure {
        result.is_pure = false;
    }

    if !check_method_args(
        statements_analyzer,
        tast_info,
        &method_id,
        functionlike_storage,
        call_expr,
        &mut template_result,
        context,
        if_body_context,
        pos,
    ) {
        return get_mixed_any();
    }

    if functionlike_storage.ignore_taints_if_true {
        tast_info.if_true_assertions.insert(
            (pos.start_offset(), pos.end_offset()),
            FxHashMap::from_iter([("hakana taints".to_string(), vec![Assertion::IgnoreTaints])]),
        );
    }

    if method_id.0 == "HH\\Shapes" && method_id.1 == "keyExists" && call_expr.1.len() == 2 {
        let expr_var_id = expression_identifier::get_extended_var_id(
            &call_expr.1[0].1,
            context.function_context.calling_class.as_ref(),
            statements_analyzer.get_file_analyzer().get_file_source(),
            statements_analyzer.get_file_analyzer().resolved_names,
        );

        let dim_var_id = expression_identifier::get_dim_id(&call_expr.1[1].1);

        if let Some(expr_var_id) = expr_var_id {
            if let Some(mut dim_var_id) = dim_var_id {
                if dim_var_id.starts_with("'") {
                    dim_var_id = dim_var_id[1..(dim_var_id.len() - 1)].to_string();
                    tast_info.if_true_assertions.insert(
                        (pos.start_offset(), pos.end_offset()),
                        FxHashMap::from_iter([(
                            format!("{}", expr_var_id),
                            vec![Assertion::HasArrayKey(dim_var_id)],
                        )]),
                    );
                } else {
                    tast_info.if_true_assertions.insert(
                        (pos.start_offset(), pos.end_offset()),
                        FxHashMap::from_iter([(
                            format!("{}[{}]", expr_var_id, dim_var_id),
                            vec![Assertion::ArrayKeyExists],
                        )]),
                    );
                }
            }
        }
    }

    if method_id.0 == "HH\\Shapes" && method_id.1 == "removeKey" && call_expr.1.len() == 2 {
        let expr_var_id = expression_identifier::get_extended_var_id(
            &call_expr.1[0].1,
            context.function_context.calling_class.as_ref(),
            statements_analyzer.get_file_analyzer().get_file_source(),
            statements_analyzer.get_file_analyzer().resolved_names,
        );
        let dim_var_id = expression_identifier::get_dim_id(&call_expr.1[1].1);

        if let (Some(expr_var_id), Some(dim_var_id)) = (expr_var_id, dim_var_id) {
            if let Some(expr_type) = context.vars_in_scope.get(&expr_var_id) {
                let mut new_type = (**expr_type).clone();

                let dim_var_id = dim_var_id[1..dim_var_id.len() - 1].to_string();

                for (_, atomic_type) in new_type.types.iter_mut() {
                    if let TAtomic::TDict {
                        known_items: Some(ref mut known_items),
                        ..
                    } = atomic_type
                    {
                        known_items.remove(&dim_var_id);
                    }
                }

                let assignment_node = DataFlowNode::get_for_assignment(
                    expr_var_id.clone(),
                    statements_analyzer.get_hpos(&call_expr.1[0].1.pos()),
                );

                for (_, parent_node) in &expr_type.parent_nodes {
                    tast_info.data_flow_graph.add_path(
                        parent_node,
                        &assignment_node,
                        PathKind::RemoveDictKey(dim_var_id.clone()),
                        None,
                        None,
                    );
                }

                new_type.parent_nodes =
                    FxHashMap::from_iter([(assignment_node.get_id().clone(), assignment_node.clone())]);

                tast_info.data_flow_graph.add_node(assignment_node);

                context.vars_in_scope.insert(expr_var_id, Rc::new(new_type));
            }
        }
    }

    if method_id.0 == "HH\\Shapes" && method_id.1 == "idx" && call_expr.1.len() >= 2 {
        let dict_type = tast_info.get_rc_expr_type(call_expr.1[0].1.pos()).cloned();
        let dim_type = tast_info.get_rc_expr_type(call_expr.1[1].1.pos()).cloned();

        let mut expr_type = None;

        if let (Some(dict_type), Some(dim_type)) = (dict_type, dim_type) {
            let mut has_valid_expected_offset = false;

            for (_, atomic_type) in &dict_type.types {
                if let TAtomic::TDict { .. } = atomic_type {
                    let mut has_possibly_undefined = false;
                    let mut expr_type_inner = handle_array_access_on_dict(
                        statements_analyzer,
                        pos,
                        tast_info,
                        context,
                        atomic_type,
                        &*dim_type,
                        false,
                        &mut has_valid_expected_offset,
                        true,
                        &mut has_possibly_undefined,
                    );

                    if has_possibly_undefined && call_expr.1.len() == 2 {
                        expr_type_inner.add_type(TAtomic::TNull);
                    }

                    expr_type = Some(expr_type_inner);
                }
            }

            if !has_valid_expected_offset && call_expr.1.len() > 2 {
                let default_type = tast_info.get_expr_type(call_expr.1[2].1.pos());
                expr_type = if let Some(expr_type) = expr_type {
                    Some(if let Some(default_type) = default_type {
                        add_union_type(expr_type, default_type, Some(codebase), false)
                    } else {
                        get_mixed_any()
                    })
                } else {
                    None
                };
            }
        }

        return expr_type.unwrap_or(get_mixed_any());
    }

    if method_id.0 == "HH\\Shapes" && (method_id.1 == "toDict" || method_id.1 == "toArray") {
        return tast_info
            .get_expr_type(call_expr.1[0].1.pos())
            .cloned()
            .unwrap_or(get_mixed_any());
    }

    let mut return_type_candidate = method_call_return_type_fetcher::fetch(
        statements_analyzer,
        tast_info,
        context,
        &method_id,
        &declaring_method_id,
        lhs_type_part,
        lhs_var_id,
        lhs_var_pos,
        functionlike_storage,
        classlike_storage,
        &template_result,
        pos,
    );

    return_type_candidate.source_function_id = Some(method_id.to_string());

    // todo check method visibility

    // todo support if_this_is type

    // todo check for method call purity

    // todo apply assertions

    // todo dispatch after method call analysis events

    return_type_candidate
}
