use std::{
    collections::{BTreeMap, HashMap, HashSet},
    rc::Rc,
};

use hakana_algebra::Clause;
use hakana_type::combine_union_types;
use oxidized::aast;

use crate::{
    formula_generator,
    reconciler::reconciler,
    scope_analyzer::ScopeAnalyzer,
    scope_context::{control_action::ControlAction, loop_scope::LoopScope, ScopeContext},
    statements_analyzer::StatementsAnalyzer,
    typed_ast::TastInfo,
};

use super::{
    control_analyzer::BreakContext, ifelse_analyzer::remove_clauses_with_mixed_vars, loop_analyzer,
    while_analyzer::get_and_expressions,
};

pub(crate) fn analyze(
    statements_analyzer: &StatementsAnalyzer,
    stmt: (&aast::Block<(), ()>, &aast::Expr<(), ()>),
    tast_info: &mut TastInfo,
    context: &mut ScopeContext,
) -> bool {
    let mut do_context = context.clone();
    do_context.break_types.push(BreakContext::Loop);
    do_context.inside_loop = true;

    let mut loop_scope = Some({
        let mut l = LoopScope::new(context.vars_in_scope.clone());
        l.protected_var_ids = context.protected_var_ids.clone();
        l
    });

    analyze_do_naively(
        statements_analyzer,
        stmt,
        tast_info,
        context,
        &mut loop_scope,
    );

    let mut mixed_var_ids = vec![];

    let loop_scope_inner = loop_scope.as_ref().unwrap();
    for (var_id, var_type) in &loop_scope_inner.parent_context_vars {
        if var_type.is_mixed() {
            mixed_var_ids.push(var_id);
        }
    }

    let cond_id = (stmt.1 .1.start_offset(), stmt.1 .1.end_offset());

    let codebase = statements_analyzer.get_codebase();

    let assertion_context =
        statements_analyzer.get_assertion_context(context.function_context.calling_class.as_ref());

    let mut while_clauses = formula_generator::get_formula(
        cond_id,
        cond_id,
        stmt.1,
        &assertion_context,
        tast_info,
        true,
        false,
    )
    .unwrap_or(vec![]);

    while_clauses = remove_clauses_with_mixed_vars(while_clauses, mixed_var_ids, cond_id);

    if while_clauses.is_empty() {
        while_clauses.push(Clause::new(
            BTreeMap::new(),
            cond_id,
            cond_id,
            Some(true),
            None,
            None,
            None,
        ));
    }

    let (analysis_result, mut inner_loop_context) = loop_analyzer::analyze(
        statements_analyzer,
        stmt.0,
        get_and_expressions(stmt.1),
        vec![],
        &mut loop_scope,
        &mut do_context,
        context,
        tast_info,
        true,
        true,
    );

    let clauses_to_simplify = {
        let mut c = context
            .clauses
            .iter()
            .map(|v| (**v).clone())
            .collect::<Vec<_>>();
        c.extend(hakana_algebra::negate_formula(while_clauses).unwrap_or(vec![]));
        c
    };

    let (negated_while_types, _) = hakana_algebra::get_truths_from_formula(
        hakana_algebra::simplify_cnf(clauses_to_simplify.iter().collect())
            .iter()
            .collect(),
        None,
        &mut HashSet::new(),
    );

    if !negated_while_types.is_empty() {
        reconciler::reconcile_keyed_types(
            &negated_while_types,
            BTreeMap::new(),
            &mut inner_loop_context,
            &mut HashSet::new(),
            &HashSet::new(),
            statements_analyzer,
            tast_info,
            stmt.1.pos(),
            true,
            false,
            &HashMap::new(),
        );
    }

    let loop_scope = &loop_scope.unwrap();

    for (var_id, var_type) in inner_loop_context.vars_in_scope {
        // if there are break statements in the loop it's not certain
        // that the loop has finished executing, so the assertions at the end
        // the loop in the while conditional may not hold
        if loop_scope.final_actions.contains(&ControlAction::Break) {
            if let Some(possibly_defined_var) =
                loop_scope.possibly_defined_loop_parent_vars.get(&var_id)
            {
                context.vars_in_scope.insert(
                    var_id.clone(),
                    Rc::new(combine_union_types(
                        &var_type,
                        possibly_defined_var,
                        Some(codebase),
                        false,
                    )),
                );
            }
        } else {
            context.vars_in_scope.insert(var_id, var_type);
        }
    }

    return analysis_result;
}

fn analyze_do_naively(
    statements_analyzer: &StatementsAnalyzer,
    stmt: (&aast::Block<(), ()>, &aast::Expr<(), ()>),
    tast_info: &mut TastInfo,
    context: &mut ScopeContext,
    loop_scope: &mut Option<LoopScope>,
) {
    let mut do_context = context.clone();

    // todo suppress some issues

    statements_analyzer.analyze(stmt.0, tast_info, &mut do_context, loop_scope);

    // todo unsupress some issues
}
