//! The operation registry.
//!
//! Every user-visible function of Ancilo is an [`Operation`]: defined once, with
//! typed input/output and JSON schemas, and exposed automatically by every
//! surface (REST, CLI, MCP, assistant). Nothing may exist in only one surface.

use std::collections::BTreeMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What an operation is allowed to touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Reads state only.
    Read,
    /// Changes Ancilo's own state (models, settings, connections).
    Manage,
}

/// The surface an operation was invoked from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    Rest,
    Cli,
    Mcp,
    Assistant,
    /// Ancilo calling itself; never needs confirmation.
    Internal,
}

/// Per-call context.
#[derive(Debug, Clone)]
pub struct OpCtx {
    pub surface: Surface,
    /// The caller explicitly confirmed a consequential operation.
    pub confirmed: bool,
}

impl OpCtx {
    pub fn new(surface: Surface) -> Self {
        Self {
            surface,
            confirmed: false,
        }
    }

    pub fn confirmed(mut self, confirmed: bool) -> Self {
        self.confirmed = confirmed;
        self
    }

    pub fn internal() -> Self {
        Self {
            surface: Surface::Internal,
            confirmed: true,
        }
    }
}

/// Static description of an operation.
#[derive(Debug, Clone, Serialize)]
pub struct OpSpec {
    pub name: &'static str,
    /// One line, imperative, shown in lists and CLI help.
    pub summary: &'static str,
    /// Longer explanation for MCP clients and the assistant: when to use it.
    pub description: &'static str,
    pub permission: Permission,
    /// Has effects that are expensive or hard to undo (large downloads,
    /// deletion, writing foreign configuration). Requires confirmation.
    pub consequential: bool,
    /// Only for the user's own surfaces (the app, the CLI) – never for a
    /// model (MCP clients, the assistant): it shows or touches what the user
    /// keeps from models, like the log of what left this computer.
    pub own: bool,
    pub input_schema: Value,
    pub output_schema: Value,
}

pub trait Operation: Send + Sync {
    fn spec(&self) -> &OpSpec;
    fn call(&self, ctx: OpCtx, input: Value) -> BoxFuture<'_, Result<Value>>;
}

/// Input type for operations without parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoInput {}

/// JSON schema of `T` as a plain JSON value.
pub fn schema_of<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or(Value::Null)
}

/// Builder for typed operations.
pub struct OpBuilder {
    name: &'static str,
    summary: &'static str,
    description: &'static str,
    permission: Permission,
    consequential: bool,
    own: bool,
}

impl OpBuilder {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            summary: "",
            description: "",
            permission: Permission::Read,
            consequential: false,
            own: false,
        }
    }

    pub fn summary(mut self, s: &'static str) -> Self {
        self.summary = s;
        self
    }

    pub fn description(mut self, s: &'static str) -> Self {
        self.description = s;
        self
    }

    pub fn manage(mut self) -> Self {
        self.permission = Permission::Manage;
        self
    }

    pub fn consequential(mut self) -> Self {
        self.consequential = true;
        self
    }

    /// Only the user's own surfaces may call it – see [`OpSpec::own`].
    pub fn own(mut self) -> Self {
        self.own = true;
        self
    }

    /// Finishes the operation with a typed async handler.
    pub fn handler<I, O, F, Fut>(self, f: F) -> Arc<dyn Operation>
    where
        I: DeserializeOwned + JsonSchema + Send + 'static,
        O: Serialize + JsonSchema + Send + 'static,
        F: Fn(OpCtx, I) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O>> + Send + 'static,
    {
        let description = if self.description.is_empty() {
            self.summary
        } else {
            self.description
        };
        Arc::new(TypedOp {
            spec: OpSpec {
                name: self.name,
                summary: self.summary,
                description,
                permission: self.permission,
                consequential: self.consequential,
                own: self.own,
                input_schema: schema_of::<I>(),
                output_schema: schema_of::<O>(),
            },
            f,
            _types: PhantomData,
        })
    }
}

struct TypedOp<I, O, F> {
    spec: OpSpec,
    f: F,
    _types: PhantomData<fn(I) -> O>,
}

impl<I, O, F, Fut> Operation for TypedOp<I, O, F>
where
    I: DeserializeOwned + JsonSchema + Send + 'static,
    O: Serialize + JsonSchema + Send + 'static,
    F: Fn(OpCtx, I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<O>> + Send + 'static,
{
    fn spec(&self) -> &OpSpec {
        &self.spec
    }

    fn call(&self, ctx: OpCtx, input: Value) -> BoxFuture<'_, Result<Value>> {
        let input = if input.is_null() {
            Value::Object(Default::default())
        } else {
            input
        };
        let parsed: std::result::Result<I, _> = serde_json::from_value(input);
        match parsed {
            Err(e) => Box::pin(async move {
                Err(Error::InvalidInput(format!(
                    "invalid input for '{}': {e}",
                    self.spec.name
                )))
            }),
            Ok(input) => {
                let fut = (self.f)(ctx, input);
                Box::pin(async move {
                    let out = fut.await?;
                    Ok(serde_json::to_value(out)?)
                })
            }
        }
    }
}

/// All operations of a running Ancilo.
#[derive(Clone, Default)]
pub struct Registry {
    ops: BTreeMap<&'static str, Arc<dyn Operation>>,
    /// Values no model ever gets (the daemon's access token): hidden in
    /// every result for MCP clients and the assistant – their models may run
    /// in the cloud.
    secrets: Vec<String>,
}

/// What a model sees instead of a secret.
pub const HIDDEN: &str = "<hidden: Ancilo's access token stays on this computer>";

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an operation. Names must be unique.
    pub fn register(&mut self, op: Arc<dyn Operation>) {
        let name = op.spec().name;
        assert!(
            !self.ops.contains_key(name),
            "operation '{name}' registered twice"
        );
        assert!(
            !op.spec().summary.is_empty(),
            "operation '{name}' needs a summary"
        );
        self.ops.insert(name, op);
    }

    /// A value results never show to a model (MCP, the assistant).
    pub fn hide(&mut self, secret: impl Into<String>) {
        let secret = secret.into();
        if !secret.is_empty() {
            self.secrets.push(secret);
        }
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Operation>> {
        self.ops.get(name)
    }

    pub fn specs(&self) -> impl Iterator<Item = &OpSpec> {
        self.ops.values().map(|op| op.spec())
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.ops.keys().copied().collect()
    }

    /// Invokes an operation by name, enforcing confirmation rules.
    pub async fn call(&self, name: &str, ctx: OpCtx, input: Value) -> Result<Value> {
        let op = self
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("unknown operation '{name}'")))?;
        let spec = op.spec();
        if spec.own && matches!(ctx.surface, Surface::Mcp | Surface::Assistant) {
            return Err(Error::NotFound(format!("unknown operation '{name}'")));
        }
        if spec.consequential && !ctx.confirmed && ctx.surface != Surface::Internal {
            return Err(Error::ConfirmationRequired(format!(
                "'{name}' has consequences ({}); repeat the call with confirmation",
                spec.summary
            )));
        }
        let to_model = matches!(ctx.surface, Surface::Mcp | Surface::Assistant);
        let out = op.call(ctx, input).await?;
        Ok(if to_model {
            self.without_secrets(out)
        } else {
            out
        })
    }

    fn without_secrets(&self, v: Value) -> Value {
        match v {
            Value::String(s) => Value::String(
                self.secrets
                    .iter()
                    .fold(s, |s, secret| s.replace(secret.as_str(), HIDDEN)),
            ),
            Value::Array(a) => {
                Value::Array(a.into_iter().map(|v| self.without_secrets(v)).collect())
            }
            Value::Object(o) => Value::Object(
                o.into_iter()
                    .map(|(k, v)| (k, self.without_secrets(v)))
                    .collect(),
            ),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // covers: M11-AC-02
    /// What the user keeps from models is not there for them at all.
    #[tokio::test]
    async fn the_users_own_operations_do_not_exist_for_models() {
        let mut r = Registry::new();
        r.register(
            OpBuilder::new("diary")
                .summary("The user's own")
                .own()
                .handler(|_ctx, _i: NoInput| async move { Ok("mine") }),
        );
        for surface in [Surface::Mcp, Surface::Assistant] {
            let e = r
                .call("diary", OpCtx::new(surface), json!({}))
                .await
                .unwrap_err();
            assert!(e.to_string().contains("unknown operation"), "{e}");
        }
        for surface in [Surface::Cli, Surface::Rest, Surface::Internal] {
            assert_eq!(
                r.call("diary", OpCtx::new(surface), json!({}))
                    .await
                    .unwrap(),
                "mine"
            );
        }
    }

    /// A model never gets a secret – not through MCP, not as the
    /// assistant's tool result; the CLI and the app do.
    #[tokio::test]
    async fn secrets_never_reach_a_model() {
        let mut r = Registry::new();
        r.register(
            OpBuilder::new("config")
                .summary("Settings with the token")
                .handler(|_ctx, _i: NoInput| async move {
                    Ok(json!({"env": {"KEY": "tok-123"}, "list": ["x tok-123 y"]}))
                }),
        );
        r.hide("tok-123");
        for surface in [Surface::Mcp, Surface::Assistant] {
            let v = r
                .call("config", OpCtx::new(surface), json!({}))
                .await
                .unwrap();
            assert!(!v.to_string().contains("tok-123"), "{surface:?}: {v}");
            assert_eq!(v["env"]["KEY"], HIDDEN);
        }
        for surface in [Surface::Cli, Surface::Rest, Surface::Internal] {
            let v = r
                .call("config", OpCtx::new(surface), json!({}))
                .await
                .unwrap();
            assert_eq!(v["env"]["KEY"], "tok-123");
        }
    }

    #[derive(Deserialize, JsonSchema)]
    struct Add {
        a: i64,
        b: i64,
    }

    fn registry() -> Registry {
        let mut r = Registry::new();
        r.register(
            OpBuilder::new("add")
                .summary("Add two numbers")
                .handler(|_ctx, i: Add| async move { Ok(i.a + i.b) }),
        );
        r.register(
            OpBuilder::new("wipe")
                .summary("Delete everything")
                .manage()
                .consequential()
                .handler(|_ctx, _i: NoInput| async move { Ok("wiped") }),
        );
        r
    }

    #[tokio::test]
    async fn calls_typed_operation() {
        let out = registry()
            .call("add", OpCtx::new(Surface::Rest), json!({"a": 2, "b": 3}))
            .await
            .unwrap();
        assert_eq!(out, json!(5));
    }

    #[tokio::test]
    async fn rejects_invalid_input() {
        let err = registry()
            .call("add", OpCtx::new(Surface::Rest), json!({"a": "x"}))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "invalid_input");
    }

    #[tokio::test]
    async fn consequential_needs_confirmation() {
        let r = registry();
        let err = r
            .call("wipe", OpCtx::new(Surface::Mcp), json!({}))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "confirmation_required");
        let ok = r
            .call("wipe", OpCtx::new(Surface::Mcp).confirmed(true), json!({}))
            .await
            .unwrap();
        assert_eq!(ok, json!("wiped"));
        let internal = r.call("wipe", OpCtx::internal(), Value::Null).await;
        assert!(internal.is_ok());
    }

    #[test]
    fn exposes_schemas() {
        let r = registry();
        let spec = r.get("add").unwrap().spec();
        assert_eq!(spec.input_schema["properties"]["a"]["type"], "integer");
    }
}
