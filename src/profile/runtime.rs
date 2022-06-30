use std::collections::HashMap;

use crate::{
    profile::{Definitions, Expression, TagPattern, Value},
    tags::{CompactString, EmptyTagSource, TagDict, TagDictId, TagSource, UNKNOWN_TAG_ID},
};

use super::{NestedScope, Profile};

#[derive(Debug, Clone)]
enum BlockTy {
    Any,
    All,
    None,
    Sum,
}

#[derive(Debug, Clone)]
struct WhenBlockClause(Expr, Expr);

#[derive(Debug, Clone)]
enum Expr {
    Literal(Value),
    LookupConstant(u8),
    LookupOrCompute(u16, Box<Expr>),
    Block(BlockTy, Vec<Expr>),
    When(Vec<WhenBlockClause>),
    TagPattern(Vec<TagPattern<TagDictId>>),
}

type VariableId = u16;

struct VariableMapping {
    // ident -> id
    ids: NestedScope<CompactString, VariableId>,
    // ident -> definition
    defs: NestedScope<CompactString, Expr>,

    next_id: VariableId,
}

impl VariableMapping {
    fn new() -> Self {
        Self {
            ids: NestedScope::empty(),
            defs: NestedScope::empty(),
            next_id: VariableId::default(),
        }
    }

    fn clear(&mut self) -> Self {
        std::mem::replace(self, Self::new())
    }

    fn push(&mut self) {
        self.ids.push();
        self.defs.push();
    }

    fn pop(&mut self) {
        self.ids.pop();
        self.defs.pop();
    }

    fn add_variable(&mut self, ident: &CompactString) -> VariableId {
        let new_id = self.next_id;
        self.ids.set(ident.clone(), new_id);
        self.next_id += 1;

        new_id
    }

    fn get_or_assign_id(&mut self, ident: &CompactString) -> VariableId {
        match self.ids.get(ident) {
            Some(&id) => id,
            None => self.add_variable(ident),
        }
    }

    fn add_definition(&mut self, ident: &CompactString, expr: Expr) {
        self.defs.set(ident.clone(), expr);
    }

    fn get_definition(&self, ident: &CompactString) -> Option<&Expr> {
        self.defs.get(ident)
    }
}

struct Builder<'a> {
    constants: HashMap<CompactString, u8>,
    tag_dict: &'a TagDict<CompactString>,
    variables: VariableMapping,
}

impl<'a> Builder<'a> {
    fn new(constants: &Definitions, tag_dict: &'a TagDict<CompactString>) -> Self {
        Self {
            tag_dict,
            variables: VariableMapping::new(),
            constants: Self::build_const_map(constants),
        }
    }

    fn compact_tag(&self, key: &CompactString) -> TagDictId {
        self.tag_dict.to_compact(key).unwrap_or(UNKNOWN_TAG_ID)
    }

    fn build_const_map(defs: &Definitions) -> HashMap<CompactString, u8> {
        if defs.len() >= (u8::MAX as usize) {
            panic!("Too many constants defined")
        }

        defs.iter()
            .enumerate()
            .map(|(id, (ident, _))| (ident.clone(), id as u8))
            .collect()
    }

    fn lower(&mut self, expr: &Expression) -> Expr {
        match expr {
            Expression::Literal(val) => Expr::Literal(*val),

            Expression::Ident(ident) => {
                if let Some(def) = self.variables.get_definition(ident) {
                    let def = Box::new(def.clone());
                    let var_id = self.variables.get_or_assign_id(ident);

                    Expr::LookupOrCompute(var_id, def)
                } else if let Some(const_id) = self.constants.get(ident) {
                    Expr::LookupConstant(*const_id)
                } else {
                    panic!("undefined var or const: {:?}", ident);
                }
            }

            Expression::TagPattern(patterns) => {
                use TagPattern::*;
                let patterns = patterns
                    .iter()
                    .map(|pat| match pat {
                        Exists(k) => Exists(self.compact_tag(k)),
                        NotExists(k) => NotExists(self.compact_tag(k)),
                        OneOf(k, vs) => OneOf(
                            self.compact_tag(k),
                            vs.iter().map(|v| self.compact_tag(v)).collect(),
                        ),
                        NoneOf(k, vs) => NoneOf(
                            self.compact_tag(k),
                            vs.iter().map(|v| self.compact_tag(v)).collect(),
                        ),
                    })
                    .collect();

                Expr::TagPattern(patterns)
            }

            Expression::NamedBlock(block) => {
                self.variables.push();

                for (ident, expr) in &block.defs {
                    let expr = self.lower(expr);
                    self.variables.add_definition(ident, expr);
                }

                let ty = match block.name.as_str() {
                    "any?" => BlockTy::Any,
                    "all?" => BlockTy::All,
                    "none?" => BlockTy::None,
                    "sum" => BlockTy::Sum,
                    other => panic!("unknown block type: {:?}", other),
                };

                let body = block.body.iter().map(|expr| self.lower(expr)).collect();

                self.variables.pop();

                Expr::Block(ty, body)
            }

            Expression::WhenBlock(clauses) => Expr::When(
                clauses
                    .0
                    .iter()
                    .map(|clause| {
                        let cond = self.lower(&clause.condition);
                        let value = self.lower(&clause.value);

                        WhenBlockClause(cond, value)
                    })
                    .collect(),
            ),
        }
    }

    fn build(&mut self, expr: &Expression) -> RunnableExpr {
        let expr = self.lower(expr);
        let variables = self.variables.clear();

        RunnableExpr { expr, variables }
    }
}

struct RunnableExpr {
    expr: Expr,
    variables: VariableMapping,
}

pub struct ProfileRuntime {
    constants: Vec<Value>,

    way_penalty: Option<RunnableExpr>,
    node_penalty: Option<RunnableExpr>,
    cost_factor: Option<RunnableExpr>,
}

impl ProfileRuntime {
    pub fn from(profile: &Profile, tag_dict: &TagDict<CompactString>) -> Result<Self, ()> {
        let mut builder = Builder::new(&profile.constant_defs, tag_dict);

        Ok(Self {
            constants: Self::evaluate_constants(&mut builder, &profile.constant_defs)?,

            // TODO: oof.
            way_penalty: match &profile.way_penalty {
                Some(expr) => Some(builder.build(&Expression::NamedBlock(expr.clone()))),
                None => None,
            },
            node_penalty: match &profile.node_penalty {
                Some(expr) => Some(builder.build(&Expression::NamedBlock(expr.clone()))),
                None => None,
            },
            cost_factor: match &profile.cost_factor {
                Some(expr) => Some(builder.build(&Expression::NamedBlock(expr.clone()))),
                None => None,
            },
        })
    }

    fn evaluate_constants(builder: &mut Builder, defs: &Definitions) -> Result<Vec<Value>, ()> {
        let mut consts = vec![];

        for (ident, def) in defs {
            let runnable = builder.build(def);

            let mut context = EvalContext::constant(&consts);
            let value = context.evaluate(&runnable.expr)?;

            println!("const {:?} = {:?}", ident, def);

            consts.push(value);
        }

        Ok(consts)
    }

    fn constant_ctx(&self) -> EvalContext<'_, EmptyTagSource> {
        EvalContext::constant(&self.constants)
    }

    fn expr_ctx<'a, T>(&'a self, expr: &RunnableExpr, tag_source: &'a T) -> EvalContext<'a, T>
    where
        T: TagSource<TagDictId, TagDictId>,
    {
        EvalContext::with_tag_source(&self.constants, expr.variables.next_id as usize, tag_source)
    }

    pub fn score_way<T>(&self, tags: &T) -> Result<f32, ()>
    where
        T: TagSource<TagDictId, TagDictId>,
    {
        match &self.way_penalty {
            None => Ok(0.0),
            Some(expr) => {
                let mut context = self.expr_ctx(expr, tags);
                match context.evaluate(&expr.expr)? {
                    Value::Number(score) => Ok(score),
                    // TODO: Formally specify this somehow. Result<Option<f32>>?
                    // TODO: Can easily overflow.
                    Value::Invalid => Ok(500_000.0),
                    // TODO: recover, don't panic
                    _ => panic!("score_way returned a non-number"),
                }
            }
        }
    }
}

struct EvalContext<'a, T> {
    constants: &'a [Value],
    variables: Vec<Option<Value>>,
    tag_source: Option<&'a T>,
}

impl<'a> EvalContext<'a, EmptyTagSource> {
    fn constant(constants: &'a [Value]) -> EvalContext<'a, EmptyTagSource> {
        EvalContext {
            constants,
            variables: vec![],
            tag_source: None,
        }
    }
}

impl<'a, T> EvalContext<'a, T>
where
    T: TagSource<TagDictId, TagDictId>,
{
    fn with_tag_source(
        constants: &'a [Value],
        num_variables: usize,
        tag_source: &'a T,
    ) -> EvalContext<'a, T> {
        EvalContext {
            constants,
            variables: vec![None; num_variables],
            tag_source: Some(tag_source),
        }
    }

    fn evaluate(&mut self, expr: &Expr) -> Result<Value, ()> {
        match expr {
            Expr::Literal(val) => Ok(*val),

            Expr::LookupConstant(id) => self
                .constants
                .get(*id as usize)
                .cloned()
                .ok_or_else(|| panic!("bug: bad const lookup")),

            Expr::LookupOrCompute(id, def) => match self.variables[*id as usize] {
                Some(val) => Ok(val),
                None => {
                    let val = self.evaluate(def)?;
                    self.variables[*id as usize] = Some(val);

                    Ok(val)
                }
            },

            Expr::Block(ty, body) => self.evaluate_block(ty, body),
            Expr::When(clauses) => self.evaluate_when(clauses),
            Expr::TagPattern(patterns) => self.evaluate_tag_patterns(patterns),
        }
    }

    fn evaluate_block(&mut self, ty: &BlockTy, body: &[Expr]) -> Result<Value, ()> {
        match ty {
            BlockTy::Any => {
                for expr in body {
                    if self.evaluate(expr)?.is_truthy() {
                        return Ok(Value::Bool(true));
                    }
                }

                Ok(Value::Bool(false))
            }

            BlockTy::All => {
                for expr in body {
                    if !self.evaluate(expr)?.is_truthy() {
                        return Ok(Value::Bool(false));
                    }
                }

                Ok(Value::Bool(true))
            }

            BlockTy::None => {
                for expr in body {
                    if self.evaluate(expr)?.is_truthy() {
                        return Ok(Value::Bool(false));
                    }
                }

                Ok(Value::Bool(true))
            }

            BlockTy::Sum => {
                let mut acc = 0.0;
                for expr in body {
                    match self.evaluate(expr)? {
                        Value::Invalid => return Ok(Value::Invalid),
                        Value::Number(n) => acc += n,
                        other => panic!("Unexpected type from sum block: {:?}", other),
                    }
                }

                Ok(Value::Number(acc))
            }
        }
    }

    fn evaluate_when(&mut self, clauses: &[WhenBlockClause]) -> Result<Value, ()> {
        for clause in clauses {
            let condition = self.evaluate(&clause.0)?;
            if condition.is_truthy() {
                let value = self.evaluate(&clause.1)?;
                return Ok(value);
            }
        }

        panic!("Fallthrough -> no else block for when");
    }

    fn evaluate_tag_patterns(&mut self, patterns: &[TagPattern<TagDictId>]) -> Result<Value, ()> {
        let tag_source = self
            .tag_source
            .unwrap_or_else(|| panic!("no tags supported here"));

        for pattern in patterns {
            use TagPattern::*;
            let matches = match pattern {
                Exists(key) => tag_source.has_tag(key),

                NotExists(key) => !tag_source.has_tag(key),

                OneOf(key, values) => tag_source
                    .get_tag(key)
                    .map(|val| values.contains(val))
                    .unwrap_or(false),

                NoneOf(key, values) => tag_source
                    .get_tag(key)
                    .map(|val| !values.contains(val))
                    .unwrap_or(false),
            };

            if !matches {
                return Ok(Value::Bool(false));
            }
        }

        Ok(Value::Bool(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::tags::TagDict;

    fn common_tags() -> Vec<&'static str> {
        vec![
            "highway", "surface", "asphalt", "unpaved", "bicycle", "yes", "no", "access", "private",
        ]
    }

    #[test]
    fn build_full_runtime() {
        let input = include_str!("../../profiles/cxb.mint");
        let profile = Profile::parse(input).expect("parse success");

        let mut tag_dict = TagDict::new();
        for &tag in &common_tags() {
            tag_dict.insert(tag.into());
        }

        ProfileRuntime::from(&profile, &tag_dict).expect("create runtime");
    }

    #[test]
    fn evaluate_constants_for_runtime() {
        let input = r#"
profile "test" {
    define {
        a = 1
        b = a
        c = false
        d = any? { c; false }
        e = any? { c; false; b }
        f = sum { a; 2 }
        g = sum { invalid; a; 2 }
    }
}
"#;
        let profile = Profile::parse(input).expect("parse success");

        let mut tag_dict = TagDict::new();
        for &tag in &common_tags() {
            tag_dict.insert(tag.into());
        }

        let runtime = ProfileRuntime::from(&profile, &tag_dict).expect("create runtime");

        let expected = vec![
            Value::Number(1.0),
            Value::Number(1.0),
            Value::Bool(false),
            Value::Bool(false),
            Value::Bool(true),
            Value::Number(3.0),
            Value::Invalid,
        ];

        assert_eq!(expected, runtime.constants);
    }
}
