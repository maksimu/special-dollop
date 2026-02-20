// Multi-tab support for RBI
//
// Provides multiple browser tabs within a single RBI session.
// Ported from KCM's tab management (KCM-495).

use log::info;

/// Maximum number of tabs per session
pub const MAX_TABS: usize = 10;

/// Invalid tab ID (slot is free)
pub const TAB_ID_INVALID: i32 = -1;

/// Pending tab ID (waiting for browser to initialize)
pub const TAB_ID_PENDING: i32 = -2;

/// Information about a single tab
#[derive(Debug, Clone, Default)]
pub struct TabInfo {
    /// Internal tab ID (array index)
    pub id: usize,
    /// Browser-assigned ID (from CDP)
    pub browser_id: Option<String>,
    /// Whether this tab is currently active/visible
    pub is_active: bool,
    /// Position in tab bar (0 = leftmost)
    pub display_order: usize,
    /// Current page title
    pub page_title: String,
    /// Current URL
    pub url: String,
    /// Can navigate back in history
    pub can_go_back: bool,
    /// Can navigate forward in history
    pub can_go_forward: bool,
    /// Tab is loading
    pub is_loading: bool,
}

/// Tab manager for multi-tab RBI sessions
#[derive(Debug)]
pub struct TabManager {
    /// All tabs (sparse array - some may be None)
    tabs: Vec<Option<TabInfo>>,
    /// Currently active tab index
    active_tab: Option<usize>,
    /// Next display order for new tabs
    next_display_order: usize,
    /// Whether tab state has changed (needs sync to client)
    tabs_dirty: bool,
}

impl Default for TabManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TabManager {
    /// Create a new tab manager
    pub fn new() -> Self {
        Self {
            tabs: vec![None; MAX_TABS],
            active_tab: None,
            next_display_order: 0,
            tabs_dirty: false,
        }
    }

    /// Create a new tab
    ///
    /// Returns the tab ID if successful, None if at max capacity
    pub fn create_tab(&mut self, url: &str) -> Option<usize> {
        // Find first free slot
        let slot = self.tabs.iter().position(|t| t.is_none())?;

        let tab = TabInfo {
            id: slot,
            browser_id: None, // Will be set when browser creates page
            is_active: self.active_tab.is_none(), // First tab is active
            display_order: self.next_display_order,
            page_title: String::new(),
            url: url.to_string(),
            can_go_back: false,
            can_go_forward: false,
            is_loading: true,
        };

        if tab.is_active {
            self.active_tab = Some(slot);
        }

        self.tabs[slot] = Some(tab);
        self.next_display_order += 1;
        self.tabs_dirty = true;

        info!("RBI: Created tab {} with URL: {}", slot, url);
        Some(slot)
    }

    /// Close a tab
    pub fn close_tab(&mut self, tab_id: usize) -> bool {
        if tab_id >= self.tabs.len() {
            return false;
        }

        if self.tabs[tab_id].is_none() {
            return false;
        }

        // If closing active tab, switch to another
        if self.active_tab == Some(tab_id) {
            // Find next tab by display order
            let next = self
                .tabs
                .iter()
                .enumerate()
                .filter(|(i, t)| *i != tab_id && t.is_some())
                .min_by_key(|(_, t)| t.as_ref().unwrap().display_order)
                .map(|(i, _)| i);

            self.active_tab = next;
            if let Some(next_id) = next {
                if let Some(tab) = &mut self.tabs[next_id] {
                    tab.is_active = true;
                }
            }
        }

        self.tabs[tab_id] = None;
        self.tabs_dirty = true;

        info!("RBI: Closed tab {}", tab_id);
        true
    }

    /// Switch to a tab
    pub fn switch_to_tab(&mut self, tab_id: usize) -> bool {
        if tab_id >= self.tabs.len() || self.tabs[tab_id].is_none() {
            return false;
        }

        // Deactivate current tab
        if let Some(current) = self.active_tab {
            if let Some(tab) = &mut self.tabs[current] {
                tab.is_active = false;
            }
        }

        // Activate new tab
        if let Some(tab) = &mut self.tabs[tab_id] {
            tab.is_active = true;
        }
        self.active_tab = Some(tab_id);
        self.tabs_dirty = true;

        info!("RBI: Switched to tab {}", tab_id);
        true
    }

    /// Get active tab
    pub fn active_tab(&self) -> Option<&TabInfo> {
        self.active_tab.and_then(|id| self.tabs[id].as_ref())
    }

    /// Get active tab mutably
    pub fn active_tab_mut(&mut self) -> Option<&mut TabInfo> {
        self.active_tab.and_then(|id| self.tabs[id].as_mut())
    }

    /// Get tab by ID
    pub fn get_tab(&self, tab_id: usize) -> Option<&TabInfo> {
        self.tabs.get(tab_id).and_then(|t| t.as_ref())
    }

    /// Get tab by ID mutably
    pub fn get_tab_mut(&mut self, tab_id: usize) -> Option<&mut TabInfo> {
        self.tabs.get_mut(tab_id).and_then(|t| t.as_mut())
    }

    /// Get all active tabs
    pub fn all_tabs(&self) -> Vec<&TabInfo> {
        self.tabs.iter().filter_map(|t| t.as_ref()).collect()
    }

    /// Get tabs sorted by display order
    pub fn tabs_by_order(&self) -> Vec<&TabInfo> {
        let mut tabs: Vec<_> = self.all_tabs();
        tabs.sort_by_key(|t| t.display_order);
        tabs
    }

    /// Update tab URL (called when navigation occurs)
    pub fn update_tab_url(
        &mut self,
        tab_id: usize,
        url: &str,
        can_go_back: bool,
        can_go_forward: bool,
    ) {
        if let Some(tab) = self.get_tab_mut(tab_id) {
            tab.url = url.to_string();
            tab.can_go_back = can_go_back;
            tab.can_go_forward = can_go_forward;
            tab.is_loading = false;
            self.tabs_dirty = true;
        }
    }

    /// Update tab title
    pub fn update_tab_title(&mut self, tab_id: usize, title: &str) {
        if let Some(tab) = self.get_tab_mut(tab_id) {
            if tab.page_title != title {
                tab.page_title = title.to_string();
                self.tabs_dirty = true;
            }
        }
    }

    /// Set browser ID for tab
    pub fn set_browser_id(&mut self, tab_id: usize, browser_id: &str) {
        if let Some(tab) = self.get_tab_mut(tab_id) {
            tab.browser_id = Some(browser_id.to_string());
        }
    }

    /// Find tab by browser ID
    pub fn find_by_browser_id(&self, browser_id: &str) -> Option<usize> {
        self.tabs
            .iter()
            .enumerate()
            .find(|(_, t)| {
                t.as_ref()
                    .and_then(|t| t.browser_id.as_ref())
                    .map(|id| id == browser_id)
                    .unwrap_or(false)
            })
            .map(|(i, _)| i)
    }

    /// Check if tabs need syncing
    pub fn is_dirty(&self) -> bool {
        self.tabs_dirty
    }

    /// Mark tabs as synced
    pub fn mark_clean(&mut self) {
        self.tabs_dirty = false;
    }

    /// Get tab count
    pub fn count(&self) -> usize {
        self.tabs.iter().filter(|t| t.is_some()).count()
    }

    /// Reorder tabs
    pub fn reorder_tab(&mut self, tab_id: usize, new_position: usize) {
        let tabs: Vec<_> = self.tabs_by_order().iter().map(|t| t.id).collect();

        if tab_id >= self.tabs.len() || new_position >= tabs.len() {
            return;
        }

        // Recalculate display orders
        let mut order = 0;
        for id in tabs {
            if id == tab_id {
                continue;
            }
            if order == new_position {
                if let Some(tab) = self.get_tab_mut(tab_id) {
                    tab.display_order = order;
                }
                order += 1;
            }
            if let Some(tab) = self.get_tab_mut(id) {
                tab.display_order = order;
            }
            order += 1;
        }

        if new_position >= order {
            if let Some(tab) = self.get_tab_mut(tab_id) {
                tab.display_order = order;
            }
        }

        self.tabs_dirty = true;
    }

    /// Generate tab list instruction for Guacamole client
    ///
    /// Format: `tabs,<count>,<tab1_id>,<tab1_title>,<tab1_url>,<tab1_active>,...;`
    pub fn format_tabs_instruction(&self) -> String {
        let tabs = self.tabs_by_order();
        let count = tabs.len();

        let mut parts = vec![format!("{}", count)];

        for tab in tabs {
            parts.push(tab.id.to_string());
            parts.push(tab.page_title.clone());
            parts.push(tab.url.clone());
            parts.push(if tab.is_active { "1" } else { "0" }.to_string());
        }

        // Format with length prefixes
        let mut result = "4.tabs".to_string();
        for part in parts {
            result.push_str(&format!(",{}.{}", part.len(), part));
        }
        result.push(';');

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_tab() {
        let mut manager = TabManager::new();

        let id = manager.create_tab("https://example.com");
        assert_eq!(id, Some(0));
        assert_eq!(manager.count(), 1);

        let tab = manager.get_tab(0).unwrap();
        assert!(tab.is_active);
        assert_eq!(tab.url, "https://example.com");
    }

    #[test]
    fn test_switch_tabs() {
        let mut manager = TabManager::new();

        manager.create_tab("https://tab1.com");
        manager.create_tab("https://tab2.com");

        // First tab should be active
        assert!(manager.get_tab(0).unwrap().is_active);
        assert!(!manager.get_tab(1).unwrap().is_active);

        // Switch to tab 1
        manager.switch_to_tab(1);
        assert!(!manager.get_tab(0).unwrap().is_active);
        assert!(manager.get_tab(1).unwrap().is_active);
    }

    #[test]
    fn test_close_tab() {
        let mut manager = TabManager::new();

        manager.create_tab("https://tab1.com");
        manager.create_tab("https://tab2.com");
        manager.switch_to_tab(0);

        // Close active tab
        manager.close_tab(0);

        // Tab 1 should now be active
        assert_eq!(manager.count(), 1);
        assert!(manager.get_tab(1).unwrap().is_active);
    }

    #[test]
    fn test_max_tabs() {
        let mut manager = TabManager::new();

        for i in 0..MAX_TABS {
            assert!(manager
                .create_tab(&format!("https://tab{}.com", i))
                .is_some());
        }

        // Should fail - at capacity
        assert!(manager.create_tab("https://overflow.com").is_none());
    }

    #[test]
    fn test_tabs_instruction() {
        let mut manager = TabManager::new();
        manager.create_tab("https://example.com");

        manager.update_tab_title(0, "Example");

        let instr = manager.format_tabs_instruction();
        assert!(instr.starts_with("4.tabs,"));
        assert!(instr.contains("Example"));
    }
}
