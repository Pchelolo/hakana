use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use hakana_file_info::FileSource;
use hakana_reflection_info::{
    class_constant_info::ConstantInfo,
    classlike_info::ClassLikeInfo,
    code_location::HPos,
    codebase_info::{symbols::SymbolKind, CodebaseInfo},
    member_visibility::MemberVisibility,
    property_info::PropertyInfo,
    t_atomic::TAtomic,
    type_resolution::TypeResolutionContext,
};
use hakana_type::{get_mixed_any, wrap_atomic};
use indexmap::IndexMap;
use oxidized::{
    aast::{self, ClassConstKind},
    ast_defs::{self, ClassishKind},
};

use crate::simple_type_inferer;
use crate::typehint_resolver::get_type_from_hint;

pub(crate) fn scan(
    codebase: &mut CodebaseInfo,
    resolved_names: &FxHashMap<usize, String>,
    class_name: &String,
    classlike_node: &aast::Class_<(), ()>,
    file_source: &FileSource,
    user_defined: bool,
) -> bool {
    let mut storage = match get_classlike_storage(codebase, class_name, classlike_node, file_source)
    {
        Ok(value) => value,
        Err(value) => return value,
    };

    storage.user_defined = user_defined;

    storage.name_location = Some(HPos::new(classlike_node.name.pos(), &file_source.file_path));
    storage.def_location = Some(HPos::new(&classlike_node.span, &file_source.file_path));

    if !classlike_node.tparams.is_empty() {
        let mut type_context = TypeResolutionContext {
            template_type_map: IndexMap::new(),
            template_supers: FxHashMap::default(),
        };

        for type_param_node in classlike_node.tparams.iter() {
            type_context.template_type_map.insert(
                type_param_node.name.1.clone(),
                FxHashMap::from_iter([(class_name.clone(), Arc::new(get_mixed_any()))]),
            );
        }

        for (i, type_param_node) in classlike_node.tparams.iter().enumerate() {
            let first_constraint = type_param_node.constraints.first();

            let template_as_type = if let Some((_, constraint_hint)) = first_constraint {
                get_type_from_hint(
                    &constraint_hint.1,
                    Some(&class_name),
                    &type_context,
                    resolved_names,
                )
            } else {
                get_mixed_any()
            };

            storage
                .template_types
                .insert(type_param_node.name.1.clone(), {
                    let mut h = FxHashMap::default();
                    h.insert(class_name.clone(), Arc::new(template_as_type));
                    h
                });

            match type_param_node.variance {
                ast_defs::Variance::Covariant => {
                    storage.template_covariants.insert(i);
                }
                ast_defs::Variance::Contravariant => {
                    // todo handle this
                }
                ast_defs::Variance::Invariant => {
                    // default, do nothing

                    if class_name == "HH\\Vector" {
                        // cheat here for vectors
                        storage.template_covariants.insert(i);
                    }
                }
            }
        }
    }

    match classlike_node.kind {
        ClassishKind::Cclass(abstraction) => {
            storage.is_abstract = matches!(abstraction, ast_defs::Abstraction::Abstract);
            storage.is_final = classlike_node.final_;

            codebase
                .symbols
                .add_class_name(&class_name, Some(&file_source.file_path));

            if let Some(parent_class) = classlike_node.extends.first() {
                if let oxidized::tast::Hint_::Happly(name, params) = &*parent_class.1 {
                    let parent_name =
                        if let Some(resolved_name) = resolved_names.get(&name.0.start_offset()) {
                            resolved_name
                        } else {
                            &name.1
                        };

                    storage.direct_parent_class = Some(parent_name.clone());
                    storage.all_parent_classes.insert(parent_name.clone());

                    storage.template_extended_offsets.insert(
                        parent_name.clone(),
                        params
                            .iter()
                            .map(|param| {
                                Arc::new(get_type_from_hint(
                                    &param.1,
                                    Some(&class_name),
                                    &TypeResolutionContext {
                                        template_type_map: storage.template_types.clone(),
                                        template_supers: FxHashMap::default(),
                                    },
                                    resolved_names,
                                ))
                            })
                            .collect(),
                    );
                }
            }

            for extended_interface in &classlike_node.implements {
                if let oxidized::tast::Hint_::Happly(name, params) = &*extended_interface.1 {
                    let interface_name =
                        if let Some(resolved_name) = resolved_names.get(&name.0.start_offset()) {
                            resolved_name
                        } else {
                            &name.1
                        };

                    storage
                        .direct_class_interfaces
                        .insert(interface_name.clone());
                    storage.all_class_interfaces.insert(interface_name.clone());

                    storage.template_extended_offsets.insert(
                        interface_name.clone(),
                        params
                            .iter()
                            .map(|param| {
                                Arc::new(get_type_from_hint(
                                    &param.1,
                                    Some(&class_name),
                                    &TypeResolutionContext {
                                        template_type_map: storage.template_types.clone(),
                                        template_supers: FxHashMap::default(),
                                    },
                                    resolved_names,
                                ))
                            })
                            .collect(),
                    );
                }
            }
        }
        ClassishKind::CenumClass(abstraction) => {
            storage.is_abstract = matches!(abstraction, ast_defs::Abstraction::Abstract);
            storage.is_final = classlike_node.final_;

            storage.kind = SymbolKind::EnumClass;

            codebase
                .symbols
                .add_enum_class_name(&class_name, Some(&file_source.file_path));

            if let Some(enum_node) = &classlike_node.enum_ {
                storage.enum_type = Some(
                    get_type_from_hint(
                        &enum_node.base.1,
                        None,
                        &TypeResolutionContext::new(),
                        resolved_names,
                    )
                    .types
                    .into_iter()
                    .next()
                    .unwrap()
                    .1,
                );
            }

            // We inherit from this class so methods like `coerce` works
            let enum_class = "HH\\BuiltinEnumClass".to_string();

            storage.direct_parent_class = Some(enum_class.clone());
            storage.all_parent_classes.insert(enum_class.clone());

            let mut params = Vec::new();

            params.push(Arc::new(wrap_atomic(TAtomic::TEnum {
                name: class_name.clone(),
            })));
        }
        ClassishKind::Cinterface => {
            storage.kind = SymbolKind::Interface;
            codebase
                .symbols
                .add_interface_name(&class_name, Some(&file_source.file_path));

            for parent_interface in &classlike_node.extends {
                if let oxidized::tast::Hint_::Happly(name, params) = &*parent_interface.1 {
                    let parent_name =
                        if let Some(resolved_name) = resolved_names.get(&name.0.start_offset()) {
                            resolved_name
                        } else {
                            &name.1
                        };

                    storage.direct_parent_interfaces.insert(parent_name.clone());
                    storage.all_parent_interfaces.insert(parent_name.clone());

                    storage.template_extended_offsets.insert(
                        parent_name.clone(),
                        params
                            .iter()
                            .map(|param| {
                                Arc::new(get_type_from_hint(
                                    &param.1,
                                    Some(&class_name),
                                    &TypeResolutionContext {
                                        template_type_map: storage.template_types.clone(),
                                        template_supers: FxHashMap::default(),
                                    },
                                    resolved_names,
                                ))
                            })
                            .collect(),
                    );
                }
            }
        }
        ClassishKind::Ctrait => {
            storage.kind = SymbolKind::Trait;

            codebase
                .symbols
                .add_trait_name(&class_name, Some(&file_source.file_path));

            for req in &classlike_node.reqs {
                if let oxidized::tast::Hint_::Happly(name, params) = &*req.0 .1 {
                    let require_name =
                        if let Some(resolved_name) = resolved_names.get(&name.0.start_offset()) {
                            resolved_name
                        } else {
                            &name.1
                        };

                    match &req.1 {
                        aast::RequireKind::RequireExtends => {
                            storage.direct_parent_class = Some(require_name.clone());
                        }
                        aast::RequireKind::RequireImplements => {
                            storage
                                .direct_parent_interfaces
                                .insert(require_name.clone());
                            storage.all_parent_interfaces.insert(require_name.clone());
                        }
                        aast::RequireKind::RequireClass => todo!(),
                    };

                    storage.template_extended_offsets.insert(
                        require_name.clone(),
                        params
                            .iter()
                            .map(|param| {
                                Arc::new(get_type_from_hint(
                                    &param.1,
                                    Some(&class_name),
                                    &TypeResolutionContext {
                                        template_type_map: storage.template_types.clone(),
                                        template_supers: FxHashMap::default(),
                                    },
                                    resolved_names,
                                ))
                            })
                            .collect(),
                    );
                }
            }

            for extended_interface in &classlike_node.implements {
                if let oxidized::tast::Hint_::Happly(name, params) = &*extended_interface.1 {
                    let interface_name =
                        if let Some(resolved_name) = resolved_names.get(&name.0.start_offset()) {
                            resolved_name
                        } else {
                            &name.1
                        };

                    storage
                        .direct_class_interfaces
                        .insert(interface_name.clone());
                    storage.all_class_interfaces.insert(interface_name.clone());

                    storage.template_extended_offsets.insert(
                        interface_name.clone(),
                        params
                            .iter()
                            .map(|param| {
                                Arc::new(get_type_from_hint(
                                    &param.1,
                                    Some(&class_name),
                                    &TypeResolutionContext {
                                        template_type_map: storage.template_types.clone(),
                                        template_supers: FxHashMap::default(),
                                    },
                                    resolved_names,
                                ))
                            })
                            .collect(),
                    );
                }
            }
        }
        ClassishKind::Cenum => {
            storage.kind = SymbolKind::Enum;

            // We inherit from this class so methods like `coerce` works
            let enum_class = "HH\\BuiltinEnum".to_string();

            storage.direct_parent_class = Some(enum_class.clone());
            storage.all_parent_classes.insert(enum_class.clone());

            let mut params = Vec::new();

            params.push(Arc::new(wrap_atomic(TAtomic::TEnum {
                name: class_name.clone(),
            })));

            if let Some(enum_node) = &classlike_node.enum_ {
                storage.enum_type = Some(
                    get_type_from_hint(
                        &enum_node.base.1,
                        None,
                        &TypeResolutionContext::new(),
                        resolved_names,
                    )
                    .types
                    .into_iter()
                    .next()
                    .unwrap()
                    .1,
                );

                if let Some(constraint) = &enum_node.constraint {
                    storage.enum_constraint = Some(Box::new(
                        get_type_from_hint(
                            &constraint.1,
                            None,
                            &TypeResolutionContext::new(),
                            resolved_names,
                        )
                        .types
                        .into_iter()
                        .next()
                        .unwrap()
                        .1,
                    ));
                }
            }

            storage
                .template_extended_offsets
                .insert(enum_class.clone(), params);

            codebase
                .symbols
                .add_enum_name(&class_name, Some(&file_source.file_path));
        }
    }

    for trait_use in &classlike_node.uses {
        let trait_type = get_type_from_hint(
            &trait_use.1,
            None,
            &TypeResolutionContext {
                template_type_map: IndexMap::new(),
                template_supers: FxHashMap::default(),
            },
            resolved_names,
        )
        .types
        .into_iter()
        .next()
        .unwrap()
        .1;

        if let TAtomic::TReference { name, .. } = trait_type {
            storage.used_traits.insert(name.clone());
        }
    }

    for class_const_node in &classlike_node.consts {
        visit_class_const_declaration(
            class_const_node,
            resolved_names,
            &mut storage,
            file_source,
            &codebase,
        );
    }

    for class_typeconst_node in &classlike_node.typeconsts {
        if !class_typeconst_node.is_ctx {
            visit_class_typeconst_declaration(class_typeconst_node, resolved_names, &mut storage);
        }
    }

    for user_attribute in &classlike_node.user_attributes {
        let name = if let Some(name) = resolved_names.get(&user_attribute.name.0.start_offset()) {
            name.clone()
        } else {
            user_attribute.name.1.clone()
        };

        storage.specialize_instance = true;

        if name == "Codegen" {
            storage.generated = true;
        }

        if name == "__Sealed" {
            let mut child_classlikes = FxHashSet::default();

            for attribute_param_expr in &user_attribute.params {
                let attribute_param_type = simple_type_inferer::infer(
                    codebase,
                    &mut FxHashMap::default(),
                    attribute_param_expr,
                    resolved_names,
                );

                if let Some(attribute_param_type) = attribute_param_type {
                    for atomic in attribute_param_type.types.into_iter() {
                        if let TAtomic::TLiteralClassname { name: value }
                        | TAtomic::TLiteralString { value } = atomic.1
                        {
                            child_classlikes.insert(value);
                        }
                    }
                }
            }

            storage.child_classlikes = Some(child_classlikes);
        }
    }

    // todo iterate over enum cases

    for class_property_node in &classlike_node.vars {
        visit_property_declaration(
            class_property_node,
            resolved_names,
            &mut storage,
            file_source,
        );
    }

    for xhp_attribute in &classlike_node.xhp_attrs {
        visit_xhp_attribute(xhp_attribute, resolved_names, &mut storage, &file_source);
    }

    codebase.classlike_infos.insert(class_name.clone(), storage);

    true
}

fn visit_xhp_attribute(
    xhp_attribute: &aast::XhpAttr<(), ()>,
    resolved_names: &FxHashMap<usize, String>,
    classlike_storage: &mut ClassLikeInfo,
    file_source: &FileSource,
) {
    let mut attribute_type_location = None;
    let mut attribute_type = if let Some(hint) = &xhp_attribute.0 .1 {
        attribute_type_location = Some(HPos::new(&hint.0, &file_source.file_path));
        get_type_from_hint(
            &hint.1,
            None,
            &TypeResolutionContext {
                template_type_map: IndexMap::new(),
                template_supers: FxHashMap::default(),
            },
            resolved_names,
        )
    } else {
        get_mixed_any()
    };

    if !(if let Some(attr_tag) = &xhp_attribute.2 {
        attr_tag.is_required()
    } else {
        false
    }) && !attribute_type.is_mixed()
        && xhp_attribute.1.expr.is_none()
    {
        attribute_type.add_type(TAtomic::TNull);
    }

    let property_storage = PropertyInfo {
        is_static: false,
        visibility: MemberVisibility::Protected,
        pos: Some(HPos::new(xhp_attribute.1.id.pos(), &file_source.file_path)),
        stmt_pos: Some(HPos::new(&xhp_attribute.1.span, &file_source.file_path)),
        type_pos: attribute_type_location,
        type_: attribute_type,
        has_default: xhp_attribute.1.expr.is_some(),
        soft_readonly: false,
        is_promoted: false,
        is_internal: false,
    };

    classlike_storage
        .declaring_property_ids
        .insert(xhp_attribute.1.id.1.clone(), classlike_storage.name.clone());
    classlike_storage
        .appearing_property_ids
        .insert(xhp_attribute.1.id.1.clone(), classlike_storage.name.clone());
    classlike_storage
        .inheritable_property_ids
        .insert(xhp_attribute.1.id.1.clone(), classlike_storage.name.clone());
    classlike_storage
        .properties
        .insert(xhp_attribute.1.id.1.clone(), property_storage);
}

fn visit_class_const_declaration(
    const_node: &aast::ClassConst<(), ()>,
    resolved_names: &FxHashMap<usize, String>,
    classlike_storage: &mut ClassLikeInfo,
    file_source: &FileSource,
    codebase: &CodebaseInfo,
) {
    let mut provided_type = None;

    let mut supplied_type_location = None;

    if let Some(supplied_type_hint) = &const_node.type_ {
        provided_type = Some(get_type_from_hint(
            &*supplied_type_hint.1,
            Some(&classlike_storage.name),
            &TypeResolutionContext {
                template_type_map: classlike_storage.template_types.clone(),
                template_supers: FxHashMap::default(),
            },
            resolved_names,
        ));

        supplied_type_location = Some(HPos::new(&supplied_type_hint.0, &file_source.file_path));
    }

    let const_storage = ConstantInfo {
        pos: Some(HPos::new(const_node.id.pos(), &file_source.file_path)),
        type_pos: supplied_type_location,
        provided_type,
        inferred_type: if let ClassConstKind::CCAbstract(Some(const_expr))
        | ClassConstKind::CCConcrete(const_expr) = &const_node.kind
        {
            simple_type_inferer::infer(
                codebase,
                &mut FxHashMap::default(),
                const_expr,
                resolved_names,
            )
        } else {
            None
        },
        unresolved_value: None,
        is_abstract: matches!(const_node.kind, ClassConstKind::CCAbstract(..)),
    };

    classlike_storage
        .constants
        .insert(const_node.id.1.clone(), const_storage);
}

fn visit_class_typeconst_declaration(
    const_node: &aast::ClassTypeconstDef<(), ()>,
    resolved_names: &FxHashMap<usize, String>,
    classlike_storage: &mut ClassLikeInfo,
) {
    let const_type = match &const_node.kind {
        aast::ClassTypeconst::TCAbstract(_) => {
            return;
        }
        aast::ClassTypeconst::TCConcrete(const_node) => get_type_from_hint(
            &const_node.c_tc_type.1,
            Some(&classlike_storage.name),
            &TypeResolutionContext {
                template_type_map: classlike_storage.template_types.clone(),
                template_supers: FxHashMap::default(),
            },
            resolved_names,
        ),
    };

    classlike_storage
        .type_constants
        .insert(const_node.name.1.clone(), const_type);
}

fn visit_property_declaration(
    property_node: &aast::ClassVar<(), ()>,
    resolved_names: &FxHashMap<usize, String>,
    classlike_storage: &mut ClassLikeInfo,
    file_source: &FileSource,
) {
    let mut property_type = None;

    let mut property_type_location = None;

    if let Some(property_type_hint) = &property_node.type_.1 {
        property_type = Some(get_type_from_hint(
            &*property_type_hint.1,
            Some(&classlike_storage.name),
            &TypeResolutionContext {
                template_type_map: classlike_storage.template_types.clone(),
                template_supers: FxHashMap::default(),
            },
            resolved_names,
        ));

        property_type_location = Some(HPos::new(&property_type_hint.0, &file_source.file_path));
    }

    let property_storage = PropertyInfo {
        is_static: property_node.is_static,
        visibility: match property_node.visibility {
            ast_defs::Visibility::Private => MemberVisibility::Private,
            ast_defs::Visibility::Public | ast_defs::Visibility::Internal => {
                MemberVisibility::Public
            }
            ast_defs::Visibility::Protected => MemberVisibility::Protected,
        },
        pos: Some(HPos::new(property_node.id.pos(), &file_source.file_path)),
        stmt_pos: Some(HPos::new(&property_node.span, &file_source.file_path)),
        type_pos: property_type_location,
        type_: property_type.unwrap_or(get_mixed_any()),
        has_default: property_node.expr.is_some(),
        soft_readonly: false,
        is_promoted: false,
        is_internal: matches!(property_node.visibility, ast_defs::Visibility::Internal),
    };

    classlike_storage
        .declaring_property_ids
        .insert(property_node.id.1.clone(), classlike_storage.name.clone());

    classlike_storage
        .appearing_property_ids
        .insert(property_node.id.1.clone(), classlike_storage.name.clone());

    if !matches!(property_node.visibility, ast_defs::Visibility::Private) {
        classlike_storage
            .inheritable_property_ids
            .insert(property_node.id.1.clone(), classlike_storage.name.clone());
    }

    classlike_storage
        .properties
        .insert(property_node.id.1.clone(), property_storage);
}

fn get_classlike_storage(
    codebase: &mut CodebaseInfo,
    class_name: &String,
    //mut is_classlike_overridden: bool,
    class: &aast::Class_<(), ()>,
    file_source: &FileSource,
) -> Result<ClassLikeInfo, bool> {
    let mut storage;
    if let Some(duplicate_storage) = codebase.classlike_infos.get(class_name) {
        if !codebase.register_stub_files {
            return Err(false);
        } else {
            //is_classlike_overridden = true;

            storage = duplicate_storage.clone();
            storage.is_populated = false;
            storage.all_class_interfaces = FxHashSet::default();
            storage.direct_class_interfaces = FxHashSet::default();
            storage.is_stubbed = true;

            // todo maybe handle dependent classlikes
        }
    } else {
        storage = ClassLikeInfo {
            name: class_name.clone(),
            name_location: Some(HPos::new(class.name.pos(), &file_source.file_path)),
            ..Default::default()
        };
    }
    storage.is_user_defined = !codebase.register_stub_files;
    storage.is_stubbed = codebase.register_stub_files;
    Ok(storage)
}
