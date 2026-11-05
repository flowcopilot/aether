//! One-off helper: signs a claim + grant for a colleague's real identity in
//! the "direct" scope, since the CLI has no command for this yet (only
//! `node fixture` can produce an authorization, and only for a fake test
//! identity). Run with `cargo run --example sign_authorization -- --help`.
//!
//! What it does not do: pick capabilities, workspaces, or an approval rule
//! for you beyond the given defaults — review the printed grant before you
//! trust it.

use std::{
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use aether::{
    identity::{AccessInput, sign_access},
    node_config::grant_json,
};
use aether_core::{AccessMode, ApprovalRule};
use ed25519_dalek::SigningKey;

struct Arguments {
    issuer_key: PathBuf,
    issuer_agent_id: String,
    agent_id: String,
    endpoint_id: String,
    scope: String,
    capabilities: Vec<String>,
    workspaces: Vec<String>,
    access_mode: String,
    approval_rule: String,
    ttl_secs: u64,
    out_dir: PathBuf,
}

fn main() -> ExitCode {
    let arguments = match parse_arguments() {
        Ok(arguments) => arguments,
        Err(message) => {
            eprintln!("sign_authorization: {message}");
            print_help();
            return ExitCode::from(2);
        }
    };

    let key = match load_or_create_issuer_key(&arguments.issuer_key) {
        Ok(key) => key,
        Err(message) => {
            eprintln!("sign_authorization: {message}");
            return ExitCode::from(1);
        }
    };
    let Some(access_mode) = AccessMode::parse(&arguments.access_mode) else {
        eprintln!(
            "sign_authorization: unknown --access-mode {}",
            arguments.access_mode
        );
        return ExitCode::from(2);
    };
    let Some(approval_rule) = ApprovalRule::parse(&arguments.approval_rule) else {
        eprintln!(
            "sign_authorization: unknown --approval-rule {}",
            arguments.approval_rule
        );
        return ExitCode::from(2);
    };

    let signed = sign_access(
        &key,
        &AccessInput {
            issuer_agent_id: arguments.issuer_agent_id.clone(),
            subject_agent_id: arguments.agent_id.clone(),
            subject_endpoint_id: arguments.endpoint_id.clone(),
            scope_id: arguments.scope.clone(),
            capabilities: arguments.capabilities.clone(),
            workspaces: arguments.workspaces.clone(),
            access_mode,
            approval_rule,
            lifetime: Duration::from_secs(arguments.ttl_secs),
        },
    );
    let (authorization, grant) = match signed {
        Ok(signed) => signed,
        Err(error) => {
            eprintln!("sign_authorization: {error}");
            return ExitCode::from(1);
        }
    };

    if fs::create_dir_all(&arguments.out_dir).is_err() {
        eprintln!("sign_authorization: cannot create --out-dir");
        return ExitCode::from(1);
    }

    let authorization_path = arguments.out_dir.join("authorization.json");
    if write_json(&authorization_path, &authorization).is_err() {
        eprintln!("sign_authorization: cannot write authorization.json");
        return ExitCode::from(1);
    }

    let grant_path = arguments.out_dir.join("grant.json");
    if write_json(&grant_path, &grant_json(&grant)).is_err() {
        eprintln!("sign_authorization: cannot write grant.json");
        return ExitCode::from(1);
    }

    println!("wrote {}", authorization_path.display());
    println!("wrote {}", grant_path.display());
    println!();
    println!("send authorization.json to {}.", arguments.agent_id);
    println!("add the contents of grant.json to your policy.json's \"grants\" array.");
    println!(
        "make sure your policy.json's \"trustedIssuers\" has: \"{}\": \"{}\"",
        arguments.issuer_agent_id,
        hex::encode(key.verifying_key().to_bytes())
    );
    ExitCode::SUCCESS
}

fn load_or_create_issuer_key(path: &Path) -> Result<SigningKey, &'static str> {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|_| "cannot create --issuer-key's directory")?;
    }
    aether::discovery::load_or_create_signing_key(path)
        .map_err(|_| "cannot read or create --issuer-key")
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), ()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| ())?;
    fs::write(path, bytes).map_err(|_| ())
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut issuer_key = None;
    let mut issuer_agent_id = "owner".to_owned();
    let mut agent_id = None;
    let mut endpoint_id = None;
    let mut scope = "direct".to_owned();
    let mut capabilities = vec!["review".to_owned()];
    let mut workspaces = vec!["workspace".to_owned()];
    let mut access_mode = "read_only".to_owned();
    let mut approval_rule = "automatic".to_owned();
    let mut ttl_secs = 604_800;
    let mut out_dir = None;

    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        let mut value =
            || arguments.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--help" | "-h" => return Err("see usage below".to_owned()),
            "--issuer-key" => issuer_key = Some(PathBuf::from(value()?)),
            "--issuer-agent-id" => issuer_agent_id = value()?,
            "--agent-id" => agent_id = Some(value()?),
            "--endpoint-id" => endpoint_id = Some(value()?),
            "--scope" => scope = value()?,
            "--capability" => {
                capabilities = value()?.split(',').map(str::to_owned).collect();
            }
            "--workspace" => {
                workspaces = value()?.split(',').map(str::to_owned).collect();
            }
            "--access-mode" => access_mode = value()?,
            "--approval-rule" => approval_rule = value()?,
            "--ttl-secs" => {
                ttl_secs = value()?
                    .parse()
                    .map_err(|_| "--ttl-secs must be a number".to_owned())?;
            }
            "--out-dir" => out_dir = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown flag {other}")),
        }
    }

    Ok(Arguments {
        issuer_key: issuer_key.ok_or("--issuer-key is required")?,
        issuer_agent_id,
        agent_id: agent_id.ok_or("--agent-id is required")?,
        endpoint_id: endpoint_id.ok_or("--endpoint-id is required")?,
        scope,
        capabilities,
        workspaces,
        access_mode,
        approval_rule,
        ttl_secs,
        out_dir: out_dir.ok_or("--out-dir is required")?,
    })
}

fn print_help() {
    eprintln!(
        "usage: cargo run --example sign_authorization -- \\
    --issuer-key <path>     (created on first use, reuse it for every colleague)
    --agent-id <name>       (the colleague's chosen name)
    --endpoint-id <id>      (the colleague's `aether node endpoint` id)
    --out-dir <dir>         (where authorization.json and grant.json are written)
    [--issuer-agent-id owner] [--scope direct] [--capability review]
    [--workspace workspace] [--access-mode read_only] [--approval-rule automatic]
    [--ttl-secs 604800]"
    );
}
