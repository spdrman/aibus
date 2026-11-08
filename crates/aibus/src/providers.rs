//! The one place providers are registered. A list, not logic: a new provider adds one line.

use aibus_provider::ProviderFactory;

pub fn builtin_factories() -> Vec<Box<dyn ProviderFactory>> {
    vec![
        Box::new(aibus_provider::fake::Factory),
        // Box::new(aibus_provider_anthropic::Factory),   <- B1.5
        // Box::new(aibus_provider_openai::Factory::openai()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_fake_and_each_id_once() {
        let mut ids: Vec<_> = builtin_factories().iter().map(|f| f.id().0).collect();
        assert!(ids.contains(&"fake"));
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), builtin_factories().len());
    }
}
