use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use full_moon::{
    ast::{
        self, Assignment, Call, Expression, Field, FunctionArgs, FunctionBody, FunctionCall, Index,
        Parameter, Prefix, Stmt, Suffix, Value, Var,
    },
    parse,
    tokenizer::TokenReference,
    visitors::Visitor,
};

#[derive(Debug, Clone, Default)]
pub struct ResourceGraph {
    pub year_scripts: Vec<String>,
    pub menu_scripts: Vec<String>,
    pub room_scripts: Vec<String>,
    pub sets: Vec<SetMetadata>,
    pub actors: Vec<ActorMetadata>,
}

#[derive(Debug, Clone)]
pub struct SetMetadata {
    pub lua_file: String,
    pub variable_name: String,
    pub set_file: String,
    pub display_name: Option<String>,
    pub setup_slots: Vec<SetupSlot>,
    pub methods: Vec<SetFunction>,
}

#[derive(Debug, Clone)]
pub struct SetupSlot {
    pub label: String,
    pub index: i64,
}

#[derive(Debug, Clone)]
pub struct ActorMetadata {
    pub lua_file: String,
    pub variable_name: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct SetFunction {
    pub name: String,
    pub parameters: Vec<String>,
    pub defined_at_line: Option<usize>,
    pub defined_in: String,
    pub body: FunctionBody,
}

#[derive(Debug, Default)]
struct ResourceGraphBuilder {
    year_scripts: Vec<String>,
    menu_scripts: Vec<String>,
    room_scripts: Vec<String>,
    sets: HashMap<String, SetAccumulator>,
    actors: HashMap<String, ActorMetadata>,
}

#[derive(Debug, Clone)]
struct SetAccumulator {
    variable_name: String,
    lua_file: Option<String>,
    set_file: Option<String>,
    display_name: Option<String>,
    setup_slots: Vec<SetupSlot>,
    methods: Vec<SetFunction>,
}

impl SetAccumulator {
    fn new(variable_name: String) -> Self {
        Self {
            variable_name,
            lua_file: None,
            set_file: None,
            display_name: None,
            setup_slots: Vec::new(),
            methods: Vec::new(),
        }
    }

    fn into_metadata(mut self) -> Option<SetMetadata> {
        let set_file = match self.set_file {
            Some(value) => value,
            None => {
                eprintln!(
                    "[grim_analysis] warning: skipping set '{}' due to missing Set:create call",
                    self.variable_name
                );
                return None;
            }
        };
        let lua_file = match self.lua_file {
            Some(value) => value,
            None => {
                eprintln!(
                    "[grim_analysis] warning: skipping set '{}' due to missing source file",
                    self.variable_name
                );
                return None;
            }
        };
        self.methods.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.defined_at_line.cmp(&b.defined_at_line))
        });
        Some(SetMetadata {
            lua_file,
            variable_name: self.variable_name,
            set_file,
            display_name: self.display_name,
            setup_slots: self.setup_slots,
            methods: self.methods,
        })
    }
}

struct SetCreation {
    variable_name: String,
    set_file: String,
    display_name: Option<String>,
    setup_slots: Vec<SetupSlot>,
}

struct SetMethodRecord {
    set_variable: String,
    function: SetFunction,
}

impl ResourceGraphBuilder {
    fn ensure_set(&mut self, variable: &str) -> &mut SetAccumulator {
        self.sets
            .entry(variable.to_string())
            .or_insert_with(|| SetAccumulator::new(variable.to_string()))
    }

    fn record_set_creation(&mut self, lua_file: &str, creation: SetCreation) {
        let SetCreation {
            variable_name,
            set_file,
            display_name,
            setup_slots,
        } = creation;
        let entry = self.ensure_set(&variable_name);
        entry.lua_file = Some(lua_file.to_string());
        entry.set_file = Some(set_file);
        entry.display_name = display_name;
        entry.setup_slots = setup_slots;
    }

    fn record_set_method(&mut self, record: SetMethodRecord) {
        let entry = self.ensure_set(&record.set_variable);
        entry
            .methods
            .retain(|existing| existing.name != record.function.name);
        entry.methods.push(record.function);
    }

    fn record_actor(&mut self, actor: ActorMetadata) {
        self.actors
            .entry(actor.variable_name.clone())
            .or_insert(actor);
    }

    fn into_graph(mut self) -> ResourceGraph {
        self.year_scripts.sort();
        self.menu_scripts.sort();
        self.room_scripts.sort();

        let mut sets: Vec<SetMetadata> = self
            .sets
            .into_values()
            .filter_map(|acc| acc.into_metadata())
            .collect();
        sets.sort_by(|a, b| a.set_file.cmp(&b.set_file));

        let mut actors: Vec<ActorMetadata> = self.actors.into_values().collect();
        actors.sort_by(|a, b| a.variable_name.cmp(&b.variable_name));

        ResourceGraph {
            year_scripts: self.year_scripts,
            menu_scripts: self.menu_scripts,
            room_scripts: self.room_scripts,
            sets,
            actors,
        }
    }
}

impl ResourceGraph {
    pub fn from_data_root(root: &Path) -> Result<Self> {
        let sets_path = root.join("_sets.decompiled.lua");
        let sets_source_raw = fs::read_to_string(&sets_path)
            .with_context(|| format!("failed to read {}", sets_path.display()))?;
        let sets_source = normalize_legacy_lua(&sets_source_raw);
        let sets_ast = parse(&sets_source)
            .map_err(|error| anyhow!("failed to parse {}: {error}", sets_path.display()))?;

        let mut builder = ResourceGraphBuilder::default();
        collect_boot_lists(&sets_ast, &mut builder);

        let lua_files = collect_decompiled_lua_files(root)?;
        for path in lua_files {
            let source_raw = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let source = normalize_legacy_lua(&source_raw);
            let ast = match parse(&source) {
                Ok(ast) => ast,
                Err(error) => {
                    eprintln!(
                        "[grim_analysis] warning: skipping {} due to parse error after normalization: {}",
                        path.display(),
                        error
                    );
                    continue;
                }
            };

            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");

            process_file_ast(&ast, &relative, &mut builder);
        }

        Ok(builder.into_graph())
    }
}

fn process_file_ast(ast: &ast::Ast, relative: &str, builder: &mut ResourceGraphBuilder) {
    for stmt in ast.nodes().stmts() {
        if let Stmt::Assignment(assign) = stmt {
            if let Some(creation) = extract_set_creation(assign) {
                builder.record_set_creation(relative, creation);
            }
            if let Some(method) = extract_set_method(assign, relative) {
                builder.record_set_method(method);
            }
            if let Some(actor) = extract_actor_metadata(assign, relative) {
                builder.record_actor(actor);
            }
        }
    }
}

fn collect_boot_lists(ast: &ast::Ast, builder: &mut ResourceGraphBuilder) {
    let mut collector = BootCallCollector { builder };
    collector.visit_ast(ast);
}

struct BootCallCollector<'a> {
    builder: &'a mut ResourceGraphBuilder,
}

impl<'a> Visitor for BootCallCollector<'a> {
    fn visit_function_call(&mut self, call: &FunctionCall) {
        if let Some(args) = global_call_args(call, "dofile") {
            if let Some(script) = args.first().and_then(|expr| extract_string_literal(expr)) {
                let is_year = script.starts_with("year_");
                let is_menu = script.starts_with("menu_");
                if is_year {
                    push_unique(&mut self.builder.year_scripts, script);
                } else if is_menu {
                    push_unique(&mut self.builder.menu_scripts, script);
                }
            }
        }
        if let Some(args) = global_call_args(call, "load_room_code") {
            if let Some(script) = args.first().and_then(|expr| extract_string_literal(expr)) {
                push_unique(&mut self.builder.room_scripts, script);
            }
        }
    }
}

fn extract_set_creation(assign: &Assignment) -> Option<SetCreation> {
    let var_name = single_assignment_name(assign)?;
    let expression = assign.expressions().iter().next()?;
    let call = expression_as_function_call(expression)?;
    let args = method_call_args(call, "Set", "create")?;
    if args.len() < 3 {
        return None;
    }

    let set_file = extract_string_literal(args[0])?;
    let display_name = extract_string_literal(args[1]).filter(|s| !s.is_empty());
    let setup_slots = parse_setup_slots(args[2]);

    Some(SetCreation {
        variable_name: var_name,
        set_file,
        display_name,
        setup_slots,
    })
}

fn extract_actor_metadata(assign: &Assignment, relative: &str) -> Option<ActorMetadata> {
    let var_name = single_assignment_name(assign)?;
    let expression = assign.expressions().iter().next()?;
    let call = expression_as_function_call(expression)?;
    let args = method_call_args(call, "Actor", "create")?;
    if args.len() < 4 {
        return None;
    }

    let label = extract_string_literal(args[3])?;
    Some(ActorMetadata {
        lua_file: relative.to_string(),
        variable_name: var_name,
        label,
    })
}

fn extract_set_method(assign: &Assignment, relative: &str) -> Option<SetMethodRecord> {
    let mut vars = assign.variables().iter();
    let var = vars.next()?;
    if vars.next().is_some() {
        return None;
    }

    let var_expr = match var {
        Var::Expression(expr) => expr,
        _ => return None,
    };

    let prefix_name = match var_expr.prefix() {
        Prefix::Name(name) => name.token().to_string(),
        _ => return None,
    };

    let mut suffixes = var_expr.suffixes();
    let suffix = suffixes.next()?;
    if suffixes.next().is_some() {
        return None;
    }

    let method_name = match suffix {
        Suffix::Index(Index::Dot { name, .. }) => name.token().to_string(),
        _ => return None,
    };

    let expression = assign.expressions().iter().next()?;
    let (function_token, function_body) = expression_as_function(expression)?;

    let parameters = function_body
        .parameters()
        .iter()
        .map(|param| match param {
            Parameter::Name(token) => token.token().to_string(),
            Parameter::Ellipse(token) => token.token().to_string(),
            _ => "<unknown>".to_string(),
        })
        .collect();

    let defined_at_line = Some(function_token.start_position().line());

    Some(SetMethodRecord {
        set_variable: prefix_name,
        function: SetFunction {
            name: method_name,
            parameters,
            defined_at_line,
            defined_in: relative.to_string(),
            body: function_body.clone(),
        },
    })
}

fn single_assignment_name(assign: &Assignment) -> Option<String> {
    let mut vars = assign.variables().iter();
    let var = vars.next()?;
    if vars.next().is_some() {
        return None;
    }
    match var {
        Var::Name(token) => Some(token.token().to_string()),
        _ => None,
    }
}

fn expression_as_function_call(expr: &Expression) -> Option<&FunctionCall> {
    let expr = normalize_expression(expr);
    match expr {
        Expression::Value { value, .. } => match value.as_ref() {
            Value::FunctionCall(call) => Some(call),
            Value::ParenthesesExpression(inner) => expression_as_function_call(inner),
            _ => None,
        },
        _ => None,
    }
}

fn normalize_expression<'a>(expr: &'a Expression) -> &'a Expression {
    match expr {
        Expression::Parentheses { expression, .. } => normalize_expression(expression),
        _ => expr,
    }
}

fn method_call_args<'a>(
    call: &'a FunctionCall,
    base: &str,
    method: &str,
) -> Option<Vec<&'a Expression>> {
    if let Prefix::Name(name) = call.prefix() {
        if name.token().to_string() != base {
            return None;
        }
        for suffix in call.suffixes() {
            if let ast::Suffix::Call(Call::MethodCall(method_call)) = suffix {
                if method_call.name().token().to_string() == method {
                    return function_args_to_vec(method_call.args());
                }
            }
        }
    }
    None
}

fn global_call_args<'a>(call: &'a FunctionCall, name: &str) -> Option<Vec<&'a Expression>> {
    if let Prefix::Name(prefix) = call.prefix() {
        if prefix.token().to_string() != name {
            return None;
        }
        for suffix in call.suffixes() {
            if let ast::Suffix::Call(Call::AnonymousCall(args)) = suffix {
                return function_args_to_vec(args);
            }
        }
    }
    None
}

fn function_args_to_vec<'a>(args: &'a FunctionArgs) -> Option<Vec<&'a Expression>> {
    match args {
        FunctionArgs::Parentheses { arguments, .. } => Some(arguments.iter().collect()),
        _ => None,
    }
}

fn parse_setup_slots(expr: &Expression) -> Vec<SetupSlot> {
    if let Some(table) = expression_as_table(expr) {
        table
            .fields()
            .iter()
            .filter_map(|field| match field {
                Field::NameKey { key, value, .. } => {
                    extract_integer_literal(value).map(|index| SetupSlot {
                        label: key.token().to_string(),
                        index,
                    })
                }
                _ => None,
            })
            .collect()
    } else {
        Vec::new()
    }
}

fn expression_as_table(expr: &Expression) -> Option<&ast::TableConstructor> {
    let expr = normalize_expression(expr);
    match expr {
        Expression::Value { value, .. } => match value.as_ref() {
            Value::TableConstructor(table) => Some(table),
            Value::ParenthesesExpression(inner) => expression_as_table(inner),
            _ => None,
        },
        _ => None,
    }
}

fn expression_as_function(expr: &Expression) -> Option<(&TokenReference, &FunctionBody)> {
    let expr = normalize_expression(expr);
    match expr {
        Expression::Value { value, .. } => match value.as_ref() {
            Value::Function((token, body)) => Some((token, body)),
            Value::ParenthesesExpression(inner) => expression_as_function(inner),
            _ => None,
        },
        _ => None,
    }
}

fn extract_string_literal(expr: &Expression) -> Option<String> {
    let expr = normalize_expression(expr);
    match expr {
        Expression::Value { value, .. } => match value.as_ref() {
            Value::String(token) => Some(unquote(token)),
            Value::ParenthesesExpression(inner) => extract_string_literal(inner),
            _ => None,
        },
        _ => None,
    }
}

fn extract_integer_literal(expr: &Expression) -> Option<i64> {
    let expr = normalize_expression(expr);
    match expr {
        Expression::Value { value, .. } => match value.as_ref() {
            Value::Number(token) => parse_int_token(token),
            Value::ParenthesesExpression(inner) => extract_integer_literal(inner),
            _ => None,
        },
        _ => None,
    }
}

fn parse_int_token(token: &TokenReference) -> Option<i64> {
    let raw = token.token().to_string();
    if let Ok(value) = raw.trim().parse::<i64>() {
        return Some(value);
    }
    if let Ok(value) = raw.trim().parse::<f64>() {
        if value.fract().abs() < f64::EPSILON {
            return Some(value as i64);
        }
    }
    None
}

fn unquote(token: &TokenReference) -> String {
    let raw = token.token().to_string();
    raw.trim_matches(|c| c == '"' || c == '\'').to_string()
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !list.iter().any(|existing| existing == &value) {
        list.push(value);
    }
}

fn collect_decompiled_lua_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();

    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("failed to read directory {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("lua"))
                .unwrap_or(false)
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.contains(".decompiled"))
                    .unwrap_or(false)
            {
                files.push(path);
            }
        }
    }

    Ok(files)
}

pub fn normalize_legacy_lua(source: &str) -> String {
    #[derive(Copy, Clone)]
    enum State {
        Normal,
        LineComment,
        BlockComment(usize),
        String(u8),
        LongString(usize),
    }

    let bytes = source.as_bytes();
    let mut result = String::with_capacity(bytes.len());
    let mut i = 0usize;
    let mut state = State::Normal;

    while i < bytes.len() {
        match state {
            State::Normal => {
                let c = bytes[i];
                let remaining = &bytes[i..];
                if c == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
                    result.push_str("--");
                    i += 2;
                    if let Some((eq_count, consumed)) = read_long_start(&bytes[i..]) {
                        result.push_str(&source[i..i + consumed]);
                        i += consumed;
                        state = State::BlockComment(eq_count);
                    } else {
                        state = State::LineComment;
                    }
                    continue;
                }
                if c == b'"' || c == b'\'' {
                    result.push(c as char);
                    i += 1;
                    state = State::String(c);
                    continue;
                }
                if c == b'[' {
                    if let Some((eq_count, consumed)) = read_long_start(remaining) {
                        result.push_str(&source[i..i + consumed]);
                        i += consumed;
                        state = State::LongString(eq_count);
                        continue;
                    }
                }
                if c == b'%' {
                    i += 1;
                    continue;
                }
                if is_ident_start(c) {
                    let start = i;
                    i += 1;
                    while i < bytes.len() && is_ident_part(bytes[i]) {
                        i += 1;
                    }
                    let ident = &source[start..i];
                    if ident == "in" {
                        result.push_str("grim_in");
                    } else {
                        result.push_str(ident);
                    }
                    continue;
                }
                result.push(c as char);
                i += 1;
            }
            State::String(delim) => {
                let c = bytes[i];
                result.push(c as char);
                i += 1;
                if c == b'\\' {
                    if i < bytes.len() {
                        result.push(bytes[i] as char);
                        i += 1;
                    }
                } else if c == delim {
                    state = State::Normal;
                }
            }
            State::LineComment => {
                let c = bytes[i];
                result.push(c as char);
                i += 1;
                if c == b'\n' {
                    state = State::Normal;
                }
            }
            State::BlockComment(eq_count) => {
                if let Some(consumed) = matches_long_end(&bytes[i..], eq_count) {
                    result.push_str(&source[i..i + consumed]);
                    i += consumed;
                    state = State::Normal;
                } else {
                    result.push(bytes[i] as char);
                    i += 1;
                }
            }
            State::LongString(eq_count) => {
                if let Some(consumed) = matches_long_end(&bytes[i..], eq_count) {
                    result.push_str(&source[i..i + consumed]);
                    i += consumed;
                    state = State::Normal;
                } else {
                    result.push(bytes[i] as char);
                    i += 1;
                }
            }
        }
    }

    result
}

fn read_long_start(bytes: &[u8]) -> Option<(usize, usize)> {
    if bytes.len() < 2 || bytes[0] != b'[' {
        return None;
    }
    let mut idx = 1usize;
    let mut eq_count = 0usize;
    while idx < bytes.len() && bytes[idx] == b'=' {
        eq_count += 1;
        idx += 1;
    }
    if idx < bytes.len() && bytes[idx] == b'[' {
        Some((eq_count, idx + 1))
    } else {
        None
    }
}

fn matches_long_end(bytes: &[u8], eq_count: usize) -> Option<usize> {
    if bytes.is_empty() || bytes[0] != b']' {
        return None;
    }
    let mut idx = 1usize;
    for _ in 0..eq_count {
        if idx >= bytes.len() || bytes[idx] != b'=' {
            return None;
        }
        idx += 1;
    }
    if idx < bytes.len() && bytes[idx] == b']' {
        Some(idx + 1)
    } else {
        None
    }
}

fn is_ident_start(c: u8) -> bool {
    c == b'_' || (c as char).is_ascii_alphabetic()
}

fn is_ident_part(c: u8) -> bool {
    is_ident_start(c) || (c as char).is_ascii_digit()
}
