use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use full_moon::ast::{
    Assignment, Block, Call, Expression, Field, FunctionArgs, FunctionCall, Index, LastStmt,
    LocalAssignment, Prefix, Stmt, Suffix, Value, Var,
};

use serde::Serialize;

use crate::resources::SetFunction;

#[derive(Debug, Clone, Default, Serialize)]
pub struct FunctionSimulation {
    pub created_actors: Vec<String>,
    pub method_calls: BTreeMap<String, BTreeMap<String, usize>>,
    pub stateful_calls: BTreeMap<StateSubsystem, BTreeMap<String, BTreeMap<String, usize>>>,
    pub stateful_call_events: Vec<StatefulCallEvent>,
    pub started_scripts: Vec<String>,
    pub movie_calls: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub geometry_calls: Vec<GeometryCallEvent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub visibility_calls: Vec<VisibilityCallEvent>,
}

pub fn simulate_set_function(function: &SetFunction) -> FunctionSimulation {
    let mut builder = FunctionSimulationBuilder::default();
    analyze_block(&mut builder, function.body.block());
    builder.finish()
}

#[derive(Default)]
struct FunctionSimulationBuilder {
    created_actors: BTreeSet<String>,
    method_calls: BTreeMap<String, BTreeMap<String, usize>>,
    stateful_calls: BTreeMap<StateSubsystem, BTreeMap<String, BTreeMap<String, usize>>>,
    stateful_call_events: Vec<StatefulCallEvent>,
    geometry_calls: Vec<GeometryCallEvent>,
    visibility_calls: Vec<VisibilityCallEvent>,
    started_scripts_seen: BTreeSet<String>,
    started_scripts: Vec<String>,
    movie_calls_seen: BTreeSet<String>,
    movie_calls: Vec<String>,
}

impl FunctionSimulationBuilder {
    fn record_created_actor<S: Into<String>>(&mut self, name: S) {
        self.created_actors.insert(name.into());
    }

    fn record_method_call(&mut self, invocation: MethodInvocation) {
        let MethodInvocation {
            target,
            method,
            args,
        } = invocation;

        let target_key = target;
        let method_key = method;

        if should_ignore_method_call(&target_key, &method_key) {
            return;
        }

        if let Some(subsystem) = classify_stateful_method(&target_key, &method_key) {
            self.record_stateful_call(subsystem, target_key, method_key, args);
            return;
        }

        let entry = self.method_calls.entry(target_key).or_default();
        *entry.entry(method_key).or_insert(0) += 1;
    }

    fn record_geometry_call<S: Into<String>>(&mut self, function: S, arguments: Vec<String>) {
        self.geometry_calls.push(GeometryCallEvent {
            function: function.into(),
            arguments,
        });
    }

    fn record_visibility_call<S: Into<String>>(&mut self, function: S, arguments: Vec<String>) {
        self.visibility_calls.push(VisibilityCallEvent {
            function: function.into(),
            arguments,
        });
    }

    fn record_stateful_call(
        &mut self,
        subsystem: StateSubsystem,
        target: String,
        method: String,
        args: Vec<String>,
    ) {
        let subsystem_entry = self
            .stateful_calls
            .entry(subsystem)
            .or_insert_with(BTreeMap::new);
        let target_entry = subsystem_entry.entry(target.clone()).or_default();
        *target_entry.entry(method.clone()).or_insert(0) += 1;

        self.stateful_call_events.push(StatefulCallEvent {
            subsystem,
            target,
            method,
            arguments: args,
        });
    }

    fn record_started_script<S: Into<String>>(&mut self, script: S) {
        let script = script.into();
        if self.started_scripts_seen.insert(script.clone()) {
            self.started_scripts.push(script);
        }
    }

    fn record_movie_call<S: Into<String>>(&mut self, movie: S) {
        let movie = movie.into();
        if self.movie_calls_seen.insert(movie.clone()) {
            self.movie_calls.push(movie);
        }
    }

    fn finish(self) -> FunctionSimulation {
        FunctionSimulation {
            created_actors: self.created_actors.into_iter().collect(),
            method_calls: self.method_calls,
            stateful_calls: self.stateful_calls,
            stateful_call_events: self.stateful_call_events,
            started_scripts: self.started_scripts,
            movie_calls: self.movie_calls,
            geometry_calls: self.geometry_calls,
            visibility_calls: self.visibility_calls,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum StateSubsystem {
    Objects,
    Inventory,
    InterestActors,
    Actors,
    Audio,
    Progression,
}

impl fmt::Display for StateSubsystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateSubsystem::Objects => write!(f, "objects"),
            StateSubsystem::Inventory => write!(f, "inventory"),
            StateSubsystem::InterestActors => write!(f, "interest actors"),
            StateSubsystem::Actors => write!(f, "actors"),
            StateSubsystem::Audio => write!(f, "audio"),
            StateSubsystem::Progression => write!(f, "progression"),
        }
    }
}

fn analyze_block(builder: &mut FunctionSimulationBuilder, block: &Block) {
    for stmt in block.stmts() {
        analyze_stmt(builder, stmt);
    }
    if let Some(last) = block.last_stmt() {
        analyze_last_stmt(builder, last);
    }
}

fn analyze_last_stmt(builder: &mut FunctionSimulationBuilder, last: &LastStmt) {
    if let LastStmt::Return(ret) = last {
        for expr in ret.returns().iter() {
            analyze_expression(builder, expr);
        }
    }
}

fn analyze_stmt(builder: &mut FunctionSimulationBuilder, stmt: &Stmt) {
    match stmt {
        Stmt::Assignment(assign) => analyze_assignment(builder, assign),
        Stmt::LocalAssignment(assign) => analyze_local_assignment(builder, assign),
        Stmt::FunctionCall(call) => analyze_function_call(builder, call),
        Stmt::Do(do_block) => analyze_block(builder, do_block.block()),
        Stmt::While(while_stmt) => {
            analyze_expression(builder, while_stmt.condition());
            analyze_block(builder, while_stmt.block());
        }
        Stmt::Repeat(repeat_stmt) => {
            analyze_block(builder, repeat_stmt.block());
            analyze_expression(builder, repeat_stmt.until());
        }
        Stmt::NumericFor(numeric) => {
            analyze_expression(builder, numeric.start());
            analyze_expression(builder, numeric.end());
            if let Some(step) = numeric.step() {
                analyze_expression(builder, step);
            }
            analyze_block(builder, numeric.block());
        }
        Stmt::GenericFor(generic) => {
            for expr in generic.expressions().iter() {
                analyze_expression(builder, expr);
            }
            analyze_block(builder, generic.block());
        }
        Stmt::If(if_stmt) => analyze_if(builder, if_stmt),
        Stmt::FunctionDeclaration(func_decl) => analyze_block(builder, func_decl.body().block()),
        Stmt::LocalFunction(local_func) => analyze_block(builder, local_func.body().block()),
        _ => {}
    }
}

fn analyze_if(builder: &mut FunctionSimulationBuilder, if_stmt: &full_moon::ast::If) {
    analyze_expression(builder, if_stmt.condition());
    analyze_block(builder, if_stmt.block());
    if let Some(else_if_blocks) = if_stmt.else_if() {
        for else_if in else_if_blocks {
            analyze_expression(builder, else_if.condition());
            analyze_block(builder, else_if.block());
        }
    }
    if let Some(else_block) = if_stmt.else_block() {
        analyze_block(builder, else_block);
    }
}

fn analyze_assignment(builder: &mut FunctionSimulationBuilder, assignment: &Assignment) {
    let expressions: Vec<&Expression> = assignment.expressions().iter().collect();
    for (var, expr) in assignment.variables().iter().zip(expressions.iter()) {
        if let Some(name) = extract_simple_var_name(var) {
            if is_actor_creation(expr) {
                builder.record_created_actor(name);
            }
        }
    }
    for expr in expressions {
        analyze_expression(builder, expr);
    }
}

fn analyze_local_assignment(builder: &mut FunctionSimulationBuilder, assignment: &LocalAssignment) {
    let expressions: Vec<&Expression> = assignment.expressions().iter().collect();
    for (name, expr) in assignment.names().iter().zip(expressions.iter()) {
        if is_actor_creation(expr) {
            builder.record_created_actor(name.token().to_string());
        }
    }
    for expr in expressions {
        analyze_expression(builder, expr);
    }
}

fn analyze_expression(builder: &mut FunctionSimulationBuilder, expression: &Expression) {
    match expression {
        Expression::BinaryOperator { lhs, rhs, .. } => {
            analyze_expression(builder, lhs);
            analyze_expression(builder, rhs);
        }
        Expression::Parentheses {
            expression: inner, ..
        } => analyze_expression(builder, inner),
        Expression::UnaryOperator {
            expression: inner, ..
        } => analyze_expression(builder, inner),
        Expression::Value { value, .. } => analyze_value(builder, value),
        _ => {}
    }
}

fn analyze_value(builder: &mut FunctionSimulationBuilder, value: &Value) {
    match value {
        Value::FunctionCall(call) => analyze_function_call(builder, call),
        Value::ParenthesesExpression(expr) => analyze_expression(builder, expr),
        Value::TableConstructor(table) => analyze_table(builder, table),
        Value::Function((_, body)) => analyze_block(builder, body.block()),
        Value::Var(var) => analyze_var(builder, var),
        Value::String(_) | Value::Number(_) | Value::Symbol(_) => {}
        _ => {}
    }
}

fn analyze_var(builder: &mut FunctionSimulationBuilder, var: &Var) {
    if let Var::Expression(var_expr) = var {
        if let Prefix::Expression(expr) = var_expr.prefix() {
            analyze_expression(builder, expr);
        }
        for suffix in var_expr.suffixes() {
            match suffix {
                Suffix::Index(Index::Brackets { expression, .. }) => {
                    analyze_expression(builder, expression)
                }
                Suffix::Index(Index::Dot { .. }) => {}
                Suffix::Call(Call::AnonymousCall(args)) => analyze_function_args(builder, args),
                Suffix::Call(Call::MethodCall(method_call)) => {
                    analyze_function_args(builder, method_call.args())
                }
                _ => {}
            }
        }
    }
}

fn analyze_table(
    builder: &mut FunctionSimulationBuilder,
    table: &full_moon::ast::TableConstructor,
) {
    for field in table.fields() {
        match field {
            Field::NameKey { value, .. } => analyze_expression(builder, value),
            Field::ExpressionKey { key, value, .. } => {
                analyze_expression(builder, key);
                analyze_expression(builder, value);
            }
            Field::NoKey(value) => analyze_expression(builder, value),
            _ => {}
        }
    }
}

fn analyze_function_call(builder: &mut FunctionSimulationBuilder, call: &FunctionCall) {
    if let Some(name) = global_function_name(call) {
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "start_script" | "single_start_script" => {
                if let Some(expr) = first_argument_expression(call) {
                    if let Some(identifier) = expression_to_identifier(expr) {
                        builder.record_started_script(identifier);
                    }
                }
            }
            "runfullscreenmovie" | "startmovie" => {
                if let Some(expr) = first_argument_expression(call) {
                    if let Some(movie) = expression_to_string_literal(expr) {
                        builder.record_movie_call(movie);
                    }
                }
            }
            "makesectoractive" => {
                for suffix in call.suffixes() {
                    if let Suffix::Call(Call::AnonymousCall(args)) = suffix {
                        builder.record_geometry_call(name.clone(), function_args_to_strings(args));
                        break;
                    }
                }
            }
            "build_hotlist"
            | "get_next_visible_object"
            | "change_gaze"
            | "enable_head_control"
            | "head_control" => {
                for suffix in call.suffixes() {
                    if let Suffix::Call(Call::AnonymousCall(args)) = suffix {
                        builder
                            .record_visibility_call(name.clone(), function_args_to_strings(args));
                        break;
                    }
                }
            }
            _ => {}
        }

        if let Some((subsystem, target, arguments)) = classify_stateful_global(&lower, call) {
            builder.record_stateful_call(subsystem, target, name.clone(), arguments);
        }
    }

    if let Prefix::Expression(expr) = call.prefix() {
        analyze_expression(builder, expr);
    }
    if let Some(invocation) = method_invocation(call) {
        let method_name = invocation.method.to_ascii_lowercase();
        if method_name == "head_look_at" {
            let function_label = format!(
                "{}:{}",
                invocation.target.clone(),
                invocation.method.clone()
            );
            builder.record_visibility_call(function_label, invocation.args.clone());
        }
        builder.record_method_call(invocation);
    }

    for suffix in call.suffixes() {
        match suffix {
            Suffix::Call(Call::AnonymousCall(args)) => analyze_function_args(builder, args),
            Suffix::Call(Call::MethodCall(method_call)) => {
                analyze_function_args(builder, method_call.args())
            }
            Suffix::Index(Index::Brackets { expression, .. }) => {
                analyze_expression(builder, expression)
            }
            Suffix::Index(Index::Dot { .. }) => {}
            _ => {}
        }
    }
}

fn global_function_name(call: &FunctionCall) -> Option<String> {
    match call.prefix() {
        Prefix::Name(name) => Some(name.token().to_string()),
        Prefix::Expression(expr) => expression_to_string(expr),
        _ => None,
    }
}

fn function_call_arguments(call: &FunctionCall) -> Option<Vec<String>> {
    for suffix in call.suffixes() {
        if let Suffix::Call(Call::AnonymousCall(args)) = suffix {
            return Some(function_args_to_strings(args));
        }
    }
    None
}

fn first_argument_expression(call: &FunctionCall) -> Option<&Expression> {
    for suffix in call.suffixes() {
        if let Suffix::Call(Call::AnonymousCall(args)) = suffix {
            if let FunctionArgs::Parentheses { arguments, .. } = args {
                return arguments.iter().next();
            }
        }
    }
    None
}

fn expression_to_identifier(expr: &Expression) -> Option<String> {
    expression_to_string(expr)
}

fn expression_to_string_literal(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Value { value, .. } => match value.as_ref() {
            Value::String(token) => Some(strip_matching_quotes(token.token().to_string())),
            Value::ParenthesesExpression(inner) => expression_to_string_literal(inner),
            _ => None,
        },
        Expression::Parentheses { expression, .. } => expression_to_string_literal(expression),
        _ => None,
    }
}

fn strip_matching_quotes(value: String) -> String {
    if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        return value[1..value.len() - 1].to_string();
    }
    if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        return value[1..value.len() - 1].to_string();
    }
    value
}

fn classify_stateful_global(
    lower: &str,
    call: &FunctionCall,
) -> Option<(StateSubsystem, String, Vec<String>)> {
    let arguments = function_call_arguments(call)?;
    if arguments.is_empty() {
        return None;
    }
    let target = normalize_actor_target(&arguments[0])?;

    let subsystem = match lower {
        "setactorpos" | "set_actor_pos" | "setactorposition" | "set_actor_position" => {
            StateSubsystem::Actors
        }
        "setactorrot" | "set_actor_rot" | "setactorrotation" | "set_actor_rotation" => {
            StateSubsystem::Actors
        }
        "setactorscale" | "setscale" | "scale" | "set_actor_scale" => StateSubsystem::Actors,
        "setactorcollisionscale"
        | "setcollisionscale"
        | "collision_scale"
        | "set_actor_collision_scale" => StateSubsystem::Actors,
        _ => return None,
    };

    Some((subsystem, target, arguments))
}

fn normalize_actor_target(label: &str) -> Option<String> {
    let mut value = label
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string();
    if value.is_empty() || value == "<expr>" {
        return None;
    }
    if let Some(stripped) = value.strip_suffix(".hActor") {
        value = stripped.to_string();
    } else if let Some(stripped) = value.strip_suffix(".hactor") {
        value = stripped.to_string();
    }
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn analyze_function_args(builder: &mut FunctionSimulationBuilder, args: &FunctionArgs) {
    match args {
        FunctionArgs::Parentheses { arguments, .. } => {
            for expr in arguments.iter() {
                analyze_expression(builder, expr);
            }
        }
        FunctionArgs::TableConstructor(table) => analyze_table(builder, table),
        FunctionArgs::String(_) => {}
        _ => {}
    }
}

fn function_args_to_strings(args: &FunctionArgs) -> Vec<String> {
    match args {
        FunctionArgs::Parentheses { arguments, .. } => arguments
            .iter()
            .map(|expr| expression_to_argument_repr(expr))
            .collect(),
        FunctionArgs::TableConstructor(_) => vec!["<table>".to_string()],
        FunctionArgs::String(token) => vec![strip_matching_quotes(token.token().to_string())],
        _ => Vec::new(),
    }
}

fn is_actor_creation(expr: &Expression) -> bool {
    if let Some(call) = expression_to_function_call(expr) {
        if let Some(invocation) = method_invocation(call) {
            return invocation.target == "Actor"
                && invocation.method.eq_ignore_ascii_case("create");
        }
    }
    false
}

fn expression_to_function_call(expr: &Expression) -> Option<&FunctionCall> {
    match expr {
        Expression::Parentheses { expression, .. } => expression_to_function_call(expression),
        Expression::UnaryOperator { expression, .. } => expression_to_function_call(expression),
        Expression::Value { value, .. } => match value.as_ref() {
            Value::FunctionCall(call) => Some(call),
            Value::ParenthesesExpression(inner) => expression_to_function_call(inner),
            _ => None,
        },
        Expression::BinaryOperator { .. } => None,
        _ => None,
    }
}

#[derive(Clone)]
struct MethodInvocation {
    target: String,
    method: String,
    args: Vec<String>,
}

fn method_invocation(call: &FunctionCall) -> Option<MethodInvocation> {
    let mut target = prefix_to_string(call.prefix())?;
    let suffixes: Vec<&Suffix> = call.suffixes().collect();
    for suffix in suffixes {
        match suffix {
            Suffix::Index(Index::Dot { name, .. }) => {
                target.push('.');
                target.push_str(&name.token().to_string());
            }
            Suffix::Index(Index::Brackets { expression, .. }) => {
                target.push('[');
                if let Some(inner) = expression_to_string(expression) {
                    target.push_str(&inner);
                } else {
                    target.push('?');
                }
                target.push(']');
            }
            Suffix::Call(Call::AnonymousCall(_)) => {
                target.push_str("()");
            }
            Suffix::Call(Call::MethodCall(method_call)) => {
                return Some(MethodInvocation {
                    target,
                    method: method_call.name().token().to_string(),
                    args: function_args_to_strings(method_call.args()),
                });
            }
            _ => {}
        }
    }
    None
}

#[derive(Debug, Clone, Serialize)]
pub struct StatefulCallEvent {
    pub subsystem: StateSubsystem,
    pub target: String,
    pub method: String,
    pub arguments: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GeometryCallEvent {
    pub function: String,
    pub arguments: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VisibilityCallEvent {
    pub function: String,
    pub arguments: Vec<String>,
}

fn prefix_to_string(prefix: &Prefix) -> Option<String> {
    match prefix {
        Prefix::Name(name) => Some(name.token().to_string()),
        Prefix::Expression(expr) => expression_to_string(expr),
        _ => None,
    }
}

fn expression_to_string(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Parentheses { expression, .. } => expression_to_string(expression),
        Expression::UnaryOperator { expression, .. } => expression_to_string(expression),
        Expression::BinaryOperator { .. } => None,
        Expression::Value { value, .. } => match value.as_ref() {
            Value::String(token) | Value::Number(token) | Value::Symbol(token) => {
                Some(token.token().to_string())
            }
            Value::Var(var) => var_to_string(var),
            Value::ParenthesesExpression(inner) => expression_to_string(inner),
            _ => None,
        },
        _ => None,
    }
}

fn expression_to_argument_repr(expr: &Expression) -> String {
    if let Expression::Value { value, .. } = expr {
        match value.as_ref() {
            Value::String(token) => return strip_matching_quotes(token.token().to_string()),
            Value::Number(token) => return token.token().to_string(),
            Value::Symbol(token) => return token.token().to_string(),
            Value::Var(var) => {
                if let Some(text) = var_to_string(var) {
                    return text;
                }
            }
            Value::ParenthesesExpression(inner) => {
                return expression_to_argument_repr(inner);
            }
            _ => {}
        }
    }
    expression_to_string(expr).unwrap_or_else(|| "<expr>".to_string())
}

fn var_to_string(var: &Var) -> Option<String> {
    match var {
        Var::Name(token) => Some(token.token().to_string()),
        Var::Expression(expr) => {
            let mut target = prefix_to_string(expr.prefix())?;
            for suffix in expr.suffixes() {
                match suffix {
                    Suffix::Index(Index::Dot { name, .. }) => {
                        target.push('.');
                        target.push_str(&name.token().to_string());
                    }
                    Suffix::Index(Index::Brackets { expression, .. }) => {
                        target.push('[');
                        if let Some(inner) = expression_to_string(expression) {
                            target.push_str(&inner);
                        } else {
                            target.push('?');
                        }
                        target.push(']');
                    }
                    Suffix::Call(Call::AnonymousCall(_)) => target.push_str("()"),
                    Suffix::Call(Call::MethodCall(method_call)) => {
                        target.push(':');
                        target.push_str(&method_call.name().token().to_string());
                        target.push_str("(...)");
                    }
                    _ => {}
                }
            }
            Some(target)
        }
        _ => None,
    }
}

fn extract_simple_var_name(var: &Var) -> Option<String> {
    if let Var::Name(token) = var {
        Some(token.token().to_string())
    } else {
        None
    }
}

fn classify_stateful_method(target: &str, method: &str) -> Option<StateSubsystem> {
    let target_lower = target.to_ascii_lowercase();
    let method_lower = method.to_ascii_lowercase();

    if target_lower.ends_with(".salflowers")
        || target_lower.ends_with(".suitcase")
        || target_lower.ends_with(".olivia_obj")
        || target_lower.ends_with(".car")
        || target_lower.ends_with(".nitrogen")
        || target_lower.ends_with(".grinder")
    {
        return Some(StateSubsystem::Objects);
    }

    const OBJECT_METHODS: &[&str] = &[
        "set_object_state",
        "set_object_state_if_unset",
        "add_object_state",
        "put_in_set",
        "remove_from_set",
        "make_touchable",
        "make_untouchable",
        "set_costume",
        "set_visibility",
        "set_state",
        "set_interest",
        "set_light_state",
        "setpos",
        "set_pos",
        "set_position",
        "setrot",
        "set_rot",
        "set_facing",
        "complete_chore",
        "play_chore",
        "play_chore_looping",
        "set_softimage_pos",
    ];

    const INVENTORY_METHODS: &[&str] = &[
        "give_new_object",
        "take_object",
        "remove_object",
        "put_in_inventory",
    ];

    const ACTOR_METHODS: &[&str] = &[
        "set_turn_rate",
        "set_walk_rate",
        "follow_boxes",
        "set_collision_mode",
        "set_costume",
        "setpos",
        "setrot",
        "set_pos",
        "set_rot",
        "set_position",
        "set_face_target",
        "look_at",
        "get_costume",
        "push_costume",
        "pop_costume",
        "ignore_boxes",
        "set_head",
        "set_look_rate",
        "default",
        "free",
        "stop_drifting",
        "drive_in",
        "setactorscale",
        "setactorcollisionscale",
        "setscale",
        "setcollisionscale",
        "set_actor_scale",
        "set_actor_collision_scale",
        "scale",
        "collision_scale",
    ];

    const AUDIO_METHODS: &[&str] = &[
        "add_ambient_sfx",
        "stop_ambient_sfx",
        "play_ambient_sfx",
        "queue_ambient_sfx",
    ];

    const PROGRESSION_METHODS: &[&str] =
        &["seteligible", "haseligibilitybeenestablished", "unlock"];

    const OBJECT_METHOD_ALIASES: &[&str] = &["set_up_aftermath"];

    if method_lower == "create" && target_lower == "actor" {
        return Some(StateSubsystem::Actors);
    }

    if INVENTORY_METHODS
        .iter()
        .any(|candidate| method_lower == *candidate || method_lower.contains(candidate))
        || method_lower.starts_with("give_")
        || method_lower.starts_with("take_")
        || target_lower.contains("inventory")
    {
        return Some(StateSubsystem::Inventory);
    }

    if target_lower.contains("interest_actor")
        || target_lower.ends_with(".interest")
        || method_lower.contains("interest")
        || method_lower.contains("chore")
    {
        return Some(StateSubsystem::InterestActors);
    }

    if is_actorish_method(&target_lower, &method_lower) {
        return Some(StateSubsystem::Actors);
    }

    if is_objectish_method(&target_lower, &method_lower)
        || OBJECT_METHODS
            .iter()
            .any(|candidate| method_lower == *candidate || method_lower.starts_with(*candidate))
        || method_lower.ends_with("_state")
        || method_lower.starts_with("put_in_")
        || method_lower.contains("object_state")
        || method_lower.contains("touchable")
        || method_lower.contains("softimage")
        || OBJECT_METHOD_ALIASES
            .iter()
            .any(|candidate| method_lower == *candidate)
    {
        return Some(StateSubsystem::Objects);
    }

    if target_lower.contains("_actor")
        || target_lower.contains(":actor")
        || ACTOR_METHODS
            .iter()
            .any(|candidate| method_lower == *candidate || method_lower.contains(candidate))
        || method_lower.starts_with("set_up_actor")
        || method_lower.starts_with("set_up_meche")
        || method_lower.starts_with("set_up_glottis")
    {
        return Some(StateSubsystem::Actors);
    }

    if AUDIO_METHODS
        .iter()
        .any(|candidate| method_lower == *candidate || method_lower.contains(*candidate))
        || target_lower.contains("ambient")
        || target_lower.contains("music")
    {
        return Some(StateSubsystem::Audio);
    }

    if PROGRESSION_METHODS
        .iter()
        .any(|candidate| method_lower == *candidate || method_lower.contains(*candidate))
        || target_lower.contains("achievement")
    {
        return Some(StateSubsystem::Progression);
    }

    if target_lower == "loading_menu" && method_lower == "close" {
        return Some(StateSubsystem::Objects);
    }

    None
}

fn is_actorish_method(target_lower: &str, method_lower: &str) -> bool {
    const ACTOR_EXACT_METHODS: &[&str] = &[
        "brennis_start_idle",
        "set_up_meche",
        "check_for_raoul_setup",
        "check_glottis_volume",
        "check_evas_head",
        "cheat_tie_rope",
        "clear_hands",
        "find_sector_name",
        "get_look_rate",
        "getpos",
        "getrot",
        "in_danger_box",
        "init_actor",
        "init_glottis",
        "init_strike_stuff",
        "kill_crying",
        "moveto",
        "put_bonewagon_in_set",
        "restore_pos",
        "salcu_setup",
        "save_pos",
        "say_line",
        "set_colormap",
        "set_speech_mode",
        "set_talk_color",
        "setup_actors",
        "set_up_angelitos",
        "set_up_barrel_bees",
        "set_up_baster",
        "set_up_mechanic_objects",
        "setup_gatekeeper",
        "setup_velasco_idles",
        "shut_up",
        "start_glottis_idle",
        "start_work",
        "stop_idles",
        "strike_idles",
        "update_glottis",
        "wait_for_message",
        "work_idles",
    ];

    if ACTOR_EXACT_METHODS
        .iter()
        .any(|candidate| method_lower == *candidate)
    {
        return true;
    }

    const ACTOR_SUBSTRINGS: &[&str] = &[
        "angelito",
        "bee",
        "bonewagon",
        "copal",
        "doug",
        "gatekeeper",
        "glottis",
        "hands",
        "idle",
        "line",
        "naranja",
        "pugsy",
        "raoul",
        "salvador",
        "speech",
        "talk",
        "toto",
        "velasco",
    ];

    if ACTOR_SUBSTRINGS
        .iter()
        .any(|keyword| method_lower.contains(keyword) || target_lower.contains(keyword))
    {
        return true;
    }

    false
}

fn is_objectish_method(target_lower: &str, method_lower: &str) -> bool {
    if method_lower == "create" && target_lower != "actor" {
        return true;
    }

    const OBJECT_EXACT_METHODS: &[&str] = &[
        "activate_forklift_boxes",
        "activate_trapdoor_boxes",
        "add_digit",
        "attach",
        "camerachange",
        "choose_random_sign_point",
        "close",
        "destroy",
        "display_number",
        "display_str",
        "exit_close",
        "disable",
        "hide",
        "init",
        "init_hq",
        "init_ropes",
        "inside_use_point",
        "is_locked",
        "lock",
        "lock_display",
        "make_visible",
        "magnetize",
        "open",
        "set_boxes",
        "set_new_out_point",
        "set_visibility",
        "set_up_states",
        "set_up_switcher_door",
        "show",
        "show_modal",
        "switch_to_set",
        "update_look_point",
        "update_states",
    ];

    if OBJECT_EXACT_METHODS
        .iter()
        .any(|candidate| method_lower == *candidate)
    {
        return true;
    }

    if method_lower.starts_with("init_")
        && method_lower != "init_actor"
        && method_lower != "init_glottis"
        && !method_lower.contains("strike")
    {
        return true;
    }

    if method_lower.starts_with("set_up_") {
        const ACTOR_SET_UP_METHODS: &[&str] = &[
            "set_up_angelitos",
            "set_up_barrel_bees",
            "set_up_baster",
            "set_up_mechanic_objects",
        ];
        return !ACTOR_SET_UP_METHODS
            .iter()
            .any(|candidate| method_lower == *candidate);
    }

    if method_lower.starts_with("setup_") {
        const ACTOR_SETUP_METHODS: &[&str] =
            &["setup_actors", "setup_gatekeeper", "setup_velasco_idles"];
        return !ACTOR_SETUP_METHODS
            .iter()
            .any(|candidate| method_lower == *candidate);
    }

    if method_lower.starts_with("activate_") || method_lower.ends_with("_boxes") {
        return true;
    }

    if method_lower == "lock" || method_lower == "unlock" {
        return true;
    }

    if method_lower == "inside_use_point" || method_lower == "side_use_point" {
        return true;
    }

    if method_lower == "choose_random_sign_point" {
        return true;
    }

    if method_lower == "switch_to_set" {
        return true;
    }

    target_lower.contains("door")
        || target_lower.contains("menu")
        || target_lower.contains("field")
        || target_lower.contains("hud")
        || target_lower.contains("system")
}

fn should_ignore_method_call(_target: &str, method: &str) -> bool {
    let method_lower = method.to_ascii_lowercase();
    matches!(method_lower.as_ref(), "current_setup" | "is_open")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::SetFunction;
    use full_moon::{
        ast::{Parameter, Stmt},
        parse,
    };

    #[test]
    fn classify_inventory_methods() {
        assert_eq!(
            classify_stateful_method("manny.inventory", "give_new_object"),
            Some(StateSubsystem::Inventory)
        );
        assert_eq!(
            classify_stateful_method("Inventory", "take_object"),
            Some(StateSubsystem::Inventory)
        );
    }

    #[test]
    fn classify_interest_actor_methods() {
        assert_eq!(
            classify_stateful_method("bd.tar.interest_actor", "play_chore_looping"),
            Some(StateSubsystem::InterestActors)
        );
        assert_eq!(
            classify_stateful_method("mo.interest", "complete_chore"),
            Some(StateSubsystem::InterestActors)
        );
    }

    #[test]
    fn classify_object_methods() {
        assert_eq!(
            classify_stateful_method("ac", "set_object_state"),
            Some(StateSubsystem::Objects)
        );
        assert_eq!(
            classify_stateful_method("ac", "make_touchable"),
            Some(StateSubsystem::Objects)
        );
    }

    #[test]
    fn classify_actor_methods() {
        assert_eq!(
            classify_stateful_method("Actor", "create"),
            Some(StateSubsystem::Actors)
        );
        assert_eq!(
            classify_stateful_method("glottis_actor", "set_turn_rate"),
            Some(StateSubsystem::Actors)
        );
        assert_eq!(
            classify_stateful_method("manny", "get_costume"),
            Some(StateSubsystem::Actors)
        );
        assert_eq!(
            classify_stateful_method("ac", "stop_drifting"),
            Some(StateSubsystem::Actors)
        );
        assert_eq!(
            classify_stateful_method("bd", "drive_in"),
            Some(StateSubsystem::Actors)
        );
        assert_eq!(
            classify_stateful_method("manny", "SetActorScale"),
            Some(StateSubsystem::Actors)
        );
        assert_eq!(
            classify_stateful_method("manny", "SetActorCollisionScale"),
            Some(StateSubsystem::Actors)
        );
        assert_eq!(
            classify_stateful_method("manny", "scale"),
            Some(StateSubsystem::Actors)
        );
    }

    #[test]
    fn classify_non_stateful_returns_none() {
        assert_eq!(classify_stateful_method("bd", "unknown_helper"), None);
    }

    #[test]
    fn classify_audio_methods() {
        assert_eq!(
            classify_stateful_method("mn", "add_ambient_sfx"),
            Some(StateSubsystem::Audio)
        );
        assert_eq!(
            classify_stateful_method("ambient_control", "stop_ambient_sfx"),
            Some(StateSubsystem::Audio)
        );
    }

    #[test]
    fn classify_progression_methods() {
        assert_eq!(
            classify_stateful_method("achievement", "setEligible"),
            Some(StateSubsystem::Progression)
        );
    }

    #[test]
    fn classify_object_softimage_method() {
        assert_eq!(
            classify_stateful_method("tree_pump", "set_softimage_pos"),
            Some(StateSubsystem::Objects)
        );
        assert_eq!(
            classify_stateful_method("loading_menu", "close"),
            Some(StateSubsystem::Objects)
        );
        assert_eq!(
            classify_stateful_method("tr", "set_up_aftermath"),
            Some(StateSubsystem::Objects)
        );
    }

    #[test]
    fn simulate_groups_stateful_calls() {
        let function = parse_set_function(
            r#"
            function enter(self)
                local flag = Actor:create("flag")
                inventory:give_new_object("card")
                self.objects.box:set_object_state("open")
                interest_actor:play_chore_looping("loop")
                Manny:set_turn_rate(45)
                SetActorScale(manny.hActor, 1.25)
                SetActorCollisionScale(manny.hActor, 0.4)
                return flag
            end
            "#,
        );

        let simulation = simulate_set_function(&function);
        assert_eq!(simulation.created_actors, vec!["flag".to_string()]);

        let inventory = simulation
            .stateful_calls
            .get(&StateSubsystem::Inventory)
            .expect("inventory bucket present");
        assert_eq!(
            inventory
                .get("inventory")
                .and_then(|m| m.get("give_new_object")),
            Some(&1)
        );

        let objects = simulation
            .stateful_calls
            .get(&StateSubsystem::Objects)
            .expect("objects bucket present");
        assert_eq!(
            objects
                .get("self.objects.box")
                .and_then(|m| m.get("set_object_state")),
            Some(&1)
        );

        let interests = simulation
            .stateful_calls
            .get(&StateSubsystem::InterestActors)
            .expect("interest actor bucket present");
        assert_eq!(
            interests
                .get("interest_actor")
                .and_then(|m| m.get("play_chore_looping")),
            Some(&1)
        );

        let actors = simulation
            .stateful_calls
            .get(&StateSubsystem::Actors)
            .expect("actors bucket present");
        assert_eq!(
            actors.get("Manny").and_then(|m| m.get("set_turn_rate")),
            Some(&1)
        );
        let manny_handle = actors.get("manny").expect("manny handle bucket present");
        assert_eq!(manny_handle.get("SetActorScale"), Some(&1));
        assert_eq!(manny_handle.get("SetActorCollisionScale"), Some(&1));
    }

    #[test]
    fn simulate_ignores_read_only_calls() {
        let function = parse_set_function(
            r#"
            function enter(self)
                if self:current_setup() == 1 then
                    return
                end
                if self.tube:is_open() then
                    return
                end
            end
            "#,
        );

        let simulation = simulate_set_function(&function);
        assert!(simulation.method_calls.is_empty());
        assert!(simulation.stateful_calls.is_empty());
        assert!(simulation.started_scripts.is_empty());
        assert!(simulation.movie_calls.is_empty());
    }

    #[test]
    fn simulate_records_script_and_movie_triggers() {
        let function = parse_set_function(
            r#"
            function enter(self)
                start_script(cut_scene.intro)
                single_start_script(mo.extra_helper)
                RunFullscreenMovie("intro.snm")
                StartMovie("mo_ts.snm", nil, 0, 256)
            end
            "#,
        );

        let simulation = simulate_set_function(&function);
        assert_eq!(
            simulation.started_scripts,
            vec!["cut_scene.intro".to_string(), "mo.extra_helper".to_string()]
        );
        assert_eq!(
            simulation.movie_calls,
            vec!["intro.snm".to_string(), "mo_ts.snm".to_string()]
        );
    }

    #[test]
    fn simulate_records_visibility_calls() {
        let function = parse_set_function(
            r#"
            function enter(self)
                Build_Hotlist(self.target)
                system.currentActor:head_look_at(hot_object)
                system.currentActor:head_look_at(nil)
            end
            "#,
        );

        let simulation = simulate_set_function(&function);
        assert_eq!(simulation.visibility_calls.len(), 3);
        assert_eq!(simulation.visibility_calls[0].function, "Build_Hotlist");
        assert_eq!(
            simulation.visibility_calls[0].arguments,
            vec!["self.target".to_string()]
        );
        assert_eq!(
            simulation.visibility_calls[1].function,
            "system.currentActor:head_look_at"
        );
        assert_eq!(
            simulation.visibility_calls[1].arguments,
            vec!["hot_object".to_string()]
        );
        assert_eq!(
            simulation.visibility_calls[2].arguments,
            vec!["nil".to_string()]
        );
    }

    fn parse_set_function(lua: &str) -> SetFunction {
        let ast = parse(lua).expect("valid lua snippet");
        for stmt in ast.nodes().stmts() {
            if let Stmt::FunctionDeclaration(func) = stmt {
                let params = func
                    .body()
                    .parameters()
                    .iter()
                    .map(|param| match param {
                        Parameter::Name(token) => token.token().to_string(),
                        Parameter::Ellipse(token) => token.token().to_string(),
                        _ => "<unknown>".to_string(),
                    })
                    .collect();
                return SetFunction {
                    name: "enter".to_string(),
                    parameters: params,
                    defined_at_line: Some(1),
                    defined_in: "<test>".to_string(),
                    body: func.body().clone(),
                };
            }
        }
        panic!("no function found in test snippet");
    }
}
