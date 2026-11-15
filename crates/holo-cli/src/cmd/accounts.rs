use chrono::Utc;
use holo_core::ProviderKind;
use holo_state::AccountRecord;

use super::{Ctx, human_bytes};
use crate::providers;

fn open_browser(url: &str) {
    eprintln!("Opening the browser to sign in. If nothing happens, open:\n  {url}\n");
    #[cfg(windows)]
    let _ = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
}

/// Stable local id: `<provider>-<first 12 alphanumerics of the remote id>`.
fn account_id(kind: ProviderKind, remote_id: &str) -> String {
    let short: String = remote_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect();
    format!("{}-{}", kind.as_str(), short.to_lowercase())
}

pub async fn login(ctx: &Ctx, kind: ProviderKind) -> anyhow::Result<()> {
    let oauth = providers::oauth_config(&ctx.cfg, kind)?;
    let tokens = holo_auth::interactive_login(&oauth, &providers::http(), open_browser).await?;

    // Stage tokens under a temporary key to ask the provider who we are.
    let staging = providers::token_manager(&ctx.cfg, &ctx.paths, kind, "login-staging")?;
    staging.set(tokens.clone()).await?;
    let provider = providers::connect(&ctx.cfg, kind, staging.clone()).await?;
    let info = provider.account().await?;
    staging.logout().await?;

    let id = account_id(kind, &info.remote_id);
    providers::token_manager(&ctx.cfg, &ctx.paths, kind, &id)?
        .set(tokens)
        .await?;
    ctx.state.upsert_account(&AccountRecord {
        id: id.clone(),
        provider: kind,
        remote_id: info.remote_id,
        display_name: info.display_name.clone(),
        email: info.email.clone(),
        created_at: Utc::now(),
    })?;
    println!(
        "Signed in as {} <{}> → account id `{id}`",
        info.display_name,
        info.email.unwrap_or_default()
    );
    Ok(())
}

pub async fn logout(ctx: &Ctx, id: &str) -> anyhow::Result<()> {
    let account = ctx.account(Some(id))?;
    if ctx
        .state
        .roots()?
        .iter()
        .any(|r| r.account_id == account.id)
    {
        anyhow::bail!("account still has sync roots; remove them first (`holo roots`)");
    }
    providers::token_manager(&ctx.cfg, &ctx.paths, account.provider, &account.id)?
        .logout()
        .await?;
    ctx.state.delete_account(&account.id)?;
    println!("Signed out of {}", account.id);
    Ok(())
}

pub async fn list(ctx: &Ctx) -> anyhow::Result<()> {
    let accounts = ctx.state.accounts()?;
    if accounts.is_empty() {
        println!("No accounts. Run `holo login`.");
    }
    for a in accounts {
        let quota = match providers::for_account(&ctx.cfg, &ctx.paths, &a).await {
            Ok(p) => match p.account().await {
                Ok(i) => match (i.quota_used, i.quota_total) {
                    (Some(u), Some(t)) => format!("{} / {}", human_bytes(u), human_bytes(t)),
                    _ => "quota unknown".into(),
                },
                Err(e) => format!("error: {e}"),
            },
            Err(e) => format!("error: {e:#}"),
        };
        println!(
            "{:<28} {:<10} {:<30} {}",
            a.id,
            a.provider,
            a.email.unwrap_or(a.display_name),
            quota
        );
    }
    Ok(())
}
