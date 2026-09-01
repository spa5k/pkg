use pkg_core::lifecycle::LifecycleState;
use pkg_core::state::CollisionPolicy;
use pkg_store::{ActivationInput, ActivationPlan};
use serde_json::{Value, json};

pub fn activation_inputs(state: &LifecycleState) -> Vec<ActivationInput> {
    state
        .manifest()
        .entries()
        .iter()
        .flat_map(|entry| {
            let realization = state.locked().entries()[entry.id()].realization();
            realization.outputs_to_install().iter().map(move |output| {
                ActivationInput::bound(
                    entry.id().clone(),
                    output.clone(),
                    realization.outputs()[output].clone(),
                )
            })
        })
        .collect()
}

pub const fn collision_policy_name(policy: CollisionPolicy) -> &'static str {
    match policy {
        CollisionPolicy::Abort => "abort",
        CollisionPolicy::KeepFirst => "keep-first",
        CollisionPolicy::KeepLast => "keep-last",
    }
}

pub fn collision_resolutions(plan: &ActivationPlan) -> Option<Vec<Value>> {
    plan.collisions()
        .iter()
        .map(|collision| {
            let relative_path = collision.relative_path().to_str()?;
            let (winner_selector, winner_output) = collision.winner_choice()?;
            let losers = collision
                .loser_choices()?
                .into_iter()
                .map(|(selector, output)| {
                    json!({
                        "sourceSelector": selector.as_str(),
                        "output": output.as_str()
                    })
                })
                .collect::<Vec<_>>();
            Some(json!({
                "relativePath": relative_path,
                "winner": {
                    "sourceSelector": winner_selector.as_str(),
                    "output": winner_output.as_str()
                },
                "losers": losers
            }))
        })
        .collect()
}
