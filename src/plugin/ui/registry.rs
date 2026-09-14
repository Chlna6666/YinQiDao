use std::{
    collections::HashMap,
    sync::{Arc, OnceLock, RwLock},
};

use anyhow::{Result, anyhow};

use super::manifest::{
    PluginUiContributions, UiCommandContribution, UiHomeSectionContribution, UiPageContribution,
    UiRouteContribution, UiThemeContribution, qualified_ui_id, validate_contributions,
};

static PLUGIN_UI_REGISTRY: OnceLock<Arc<RwLock<PluginUiRegistry>>> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredUiRoute {
    pub plugin_id: String,
    pub qualified_id: String,
    pub contribution: UiRouteContribution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredUiPage {
    pub plugin_id: String,
    pub qualified_id: String,
    pub contribution: UiPageContribution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredUiCommand {
    pub plugin_id: String,
    pub qualified_id: String,
    pub contribution: UiCommandContribution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredUiHomeSection {
    pub plugin_id: String,
    pub qualified_id: String,
    pub contribution: UiHomeSectionContribution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredUiTheme {
    pub plugin_id: String,
    pub qualified_id: String,
    pub contribution: UiThemeContribution,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginUiRegistrationDelta {
    pub removed_routes: usize,
    pub removed_pages: usize,
    pub removed_commands: usize,
    pub removed_home_sections: usize,
    pub removed_themes: usize,
    pub added_routes: usize,
    pub added_pages: usize,
    pub added_commands: usize,
    pub added_home_sections: usize,
    pub added_themes: usize,
}

#[derive(Clone, Debug, Default)]
pub struct PluginUiRegistry {
    routes: HashMap<String, RegisteredUiRoute>,
    pages: HashMap<String, RegisteredUiPage>,
    commands: HashMap<String, RegisteredUiCommand>,
    home_sections: HashMap<String, RegisteredUiHomeSection>,
    themes: HashMap<String, RegisteredUiTheme>,
}

impl PluginUiRegistry {
    pub fn replace_plugin(
        &mut self,
        plugin_id: &str,
        contributions: PluginUiContributions,
    ) -> Result<PluginUiRegistrationDelta> {
        // Build the complete replacement before touching current state. After this stage every map
        // insertion is infallible, so invalid metadata can never partially unregister a live plugin.
        validate_contributions(plugin_id, &contributions)?;
        let pages = contributions
            .pages
            .into_iter()
            .map(|contribution| {
                Ok((qualified_ui_id(plugin_id, &contribution.id)?, contribution))
            })
            .collect::<Result<Vec<_>>>()?;
        let routes = contributions
            .routes
            .into_iter()
            .map(|contribution| {
                Ok((qualified_ui_id(plugin_id, &contribution.id)?, contribution))
            })
            .collect::<Result<Vec<_>>>()?;
        let commands = contributions
            .commands
            .into_iter()
            .map(|contribution| {
                Ok((qualified_ui_id(plugin_id, &contribution.id)?, contribution))
            })
            .collect::<Result<Vec<_>>>()?;
        let home_sections = contributions
            .home_sections
            .into_iter()
            .map(|contribution| {
                Ok((qualified_ui_id(plugin_id, &contribution.id)?, contribution))
            })
            .collect::<Result<Vec<_>>>()?;
        let themes = contributions
            .themes
            .into_iter()
            .map(|contribution| {
                Ok((qualified_ui_id(plugin_id, &contribution.id)?, contribution))
            })
            .collect::<Result<Vec<_>>>()?;

        let mut delta = self.remove_plugin(plugin_id);
        delta.added_routes = routes.len();
        delta.added_pages = pages.len();
        delta.added_commands = commands.len();
        delta.added_home_sections = home_sections.len();
        delta.added_themes = themes.len();

        for (qualified_id, contribution) in pages {
            self.pages.insert(
                qualified_id.clone(),
                RegisteredUiPage {
                    plugin_id: plugin_id.to_owned(),
                    qualified_id,
                    contribution,
                },
            );
        }
        for (qualified_id, contribution) in routes {
            self.routes.insert(
                qualified_id.clone(),
                RegisteredUiRoute {
                    plugin_id: plugin_id.to_owned(),
                    qualified_id,
                    contribution,
                },
            );
        }
        for (qualified_id, contribution) in commands {
            self.commands.insert(
                qualified_id.clone(),
                RegisteredUiCommand {
                    plugin_id: plugin_id.to_owned(),
                    qualified_id,
                    contribution,
                },
            );
        }
        for (qualified_id, contribution) in home_sections {
            self.home_sections.insert(
                qualified_id.clone(),
                RegisteredUiHomeSection {
                    plugin_id: plugin_id.to_owned(),
                    qualified_id,
                    contribution,
                },
            );
        }
        for (qualified_id, contribution) in themes {
            self.themes.insert(
                qualified_id.clone(),
                RegisteredUiTheme {
                    plugin_id: plugin_id.to_owned(),
                    qualified_id,
                    contribution,
                },
            );
        }

        Ok(delta)
    }

    pub fn remove_plugin(&mut self, plugin_id: &str) -> PluginUiRegistrationDelta {
        let before_routes = self.routes.len();
        let before_pages = self.pages.len();
        let before_commands = self.commands.len();
        let before_home_sections = self.home_sections.len();
        let before_themes = self.themes.len();

        self.routes.retain(|_, item| item.plugin_id != plugin_id);
        self.pages.retain(|_, item| item.plugin_id != plugin_id);
        self.commands.retain(|_, item| item.plugin_id != plugin_id);
        self.home_sections
            .retain(|_, item| item.plugin_id != plugin_id);
        self.themes.retain(|_, item| item.plugin_id != plugin_id);

        PluginUiRegistrationDelta {
            removed_routes: before_routes - self.routes.len(),
            removed_pages: before_pages - self.pages.len(),
            removed_commands: before_commands - self.commands.len(),
            removed_home_sections: before_home_sections - self.home_sections.len(),
            removed_themes: before_themes - self.themes.len(),
            ..PluginUiRegistrationDelta::default()
        }
    }

    pub fn route(&self, qualified_id: &str) -> Option<&RegisteredUiRoute> {
        self.routes.get(qualified_id)
    }

    pub fn page(&self, qualified_id: &str) -> Option<&RegisteredUiPage> {
        self.pages.get(qualified_id)
    }

    pub fn routes(&self) -> impl Iterator<Item = &RegisteredUiRoute> {
        self.routes.values()
    }

    pub fn pages(&self) -> impl Iterator<Item = &RegisteredUiPage> {
        self.pages.values()
    }

    pub fn commands(&self) -> impl Iterator<Item = &RegisteredUiCommand> {
        self.commands.values()
    }

    pub fn home_sections(&self) -> impl Iterator<Item = &RegisteredUiHomeSection> {
        self.home_sections.values()
    }

    pub fn themes(&self) -> impl Iterator<Item = &RegisteredUiTheme> {
        self.themes.values()
    }
}

pub fn initialize() -> Arc<RwLock<PluginUiRegistry>> {
    PLUGIN_UI_REGISTRY
        .get_or_init(|| Arc::new(RwLock::new(PluginUiRegistry::default())))
        .clone()
}

pub fn global() -> Option<Arc<RwLock<PluginUiRegistry>>> {
    PLUGIN_UI_REGISTRY.get().cloned()
}

pub fn unregister_plugin(plugin_id: &str) -> Result<PluginUiRegistrationDelta> {
    let registry = global().ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;
    Ok(registry
        .write()
        .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?
        .remove_plugin(plugin_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::ui::manifest::{UiPageContribution, UiRouteContribution, UiRoutePlacement};

    fn contribution(title: &str) -> PluginUiContributions {
        PluginUiContributions {
            pages: vec![UiPageContribution {
                id: "main".into(),
                title: title.into(),
            }],
            routes: vec![UiRouteContribution {
                id: "main".into(),
                title: title.into(),
                page_id: "main".into(),
                icon: None,
                placement: UiRoutePlacement::Sidebar,
                order: 0,
                required_provider_id: None,
            }],
            ..PluginUiContributions::default()
        }
    }

    #[test]
    fn replacement_is_namespaced_and_atomic_after_validation() {
        let mut registry = PluginUiRegistry::default();
        registry
            .replace_plugin("plugin.one", contribution("One"))
            .expect("first");
        registry
            .replace_plugin("plugin.two", contribution("Two"))
            .expect("second");
        assert!(registry.route("plugin:plugin.one/main").is_some());
        assert!(registry.route("plugin:plugin.two/main").is_some());

        let mut invalid = contribution("Broken");
        invalid.routes[0].page_id = "missing".into();
        assert!(registry.replace_plugin("plugin.one", invalid).is_err());
        assert_eq!(
            registry
                .route("plugin:plugin.one/main")
                .expect("original remains")
                .contribution
                .title,
            "One"
        );
    }

    #[test]
    fn unregister_removes_only_one_plugin() {
        let mut registry = PluginUiRegistry::default();
        registry
            .replace_plugin("plugin.one", contribution("One"))
            .expect("first");
        registry
            .replace_plugin("plugin.two", contribution("Two"))
            .expect("second");
        let delta = registry.remove_plugin("plugin.one");
        assert_eq!(delta.removed_routes, 1);
        assert!(registry.route("plugin:plugin.one/main").is_none());
        assert!(registry.route("plugin:plugin.two/main").is_some());
    }
}
