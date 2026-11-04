use amafu_scenario::Level;
use amafu_simulated_world::{SimulationSettings, run_scenario};

const LARGEST_WORLD_IN_TESTS: usize = 8;

fn seed_count() -> u64 {
    std::env::var("AMAFU_SEEDS").ok().and_then(|value| value.parse().ok()).unwrap_or(20)
}

#[test]
fn every_scenario_passes_in_the_simulated_world_for_many_seeds() {
    let mut failures = Vec::new();
    let in_tests = |definition: &&amafu_scenario::ScenarioDefinition| {
        definition.runs_at(Level::Simulated) && definition.node_count <= LARGEST_WORLD_IN_TESTS
    };
    for definition in amafu_scenarios::all().iter().filter(in_tests) {
        for seed in 0..seed_count() {
            let settings = SimulationSettings::with_seed(seed);
            let settings = if seed % 2 == 1 { settings.crashing_roles() } else { settings };
            if let Err(failure) = run_scenario(definition, settings) {
                failures.push(format!("{}\n{failure}", definition.name));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_seed_replays_identically() {
    let definition = amafu_scenarios::all()[0];
    let first = run_scenario(&definition, SimulationSettings::with_seed(7).crashing_roles());
    let second = run_scenario(&definition, SimulationSettings::with_seed(7).crashing_roles());
    assert_eq!(first, second);
}
