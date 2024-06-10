use std::collections::VecDeque;
use std::fmt::Debug;
use std::num::NonZeroUsize;
use std::sync::atomic::Ordering;

use color_eyre::eyre::anyhow;
use color_eyre::Result;
use getset::CopyGetters;
use getset::Getters;
use getset::MutGetters;
use getset::Setters;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use komorebi_core::Axis;
use komorebi_core::CustomLayout;
use komorebi_core::CycleDirection;
use komorebi_core::DefaultLayout;
use komorebi_core::Layout;
use komorebi_core::OperationDirection;
use komorebi_core::Rect;

use crate::container::Container;
use crate::ring::Ring;
use crate::static_config::WorkspaceConfig;
use crate::static_config::PokerConfig;
use crate::window::{should_act, Window};
use crate::REGEX_IDENTIFIERS;
use crate::window::WindowDetails;
use crate::windows_api::WindowsApi;
use crate::DEFAULT_CONTAINER_PADDING;
use crate::DEFAULT_WORKSPACE_PADDING;

#[allow(clippy::struct_field_names)]
#[derive(
    Debug, Clone, Serialize, Deserialize, Getters, CopyGetters, MutGetters, Setters, JsonSchema,
)]
pub struct Workspace {
    #[getset(get = "pub", set = "pub")]
    name: Option<String>,
    containers: Ring<Container>,
    #[getset(get = "pub", get_mut = "pub", set = "pub")]
    monocle_container: Option<Container>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[getset(get_copy = "pub", set = "pub")]
    monocle_container_restore_idx: Option<usize>,
    #[getset(get = "pub", get_mut = "pub", set = "pub")]
    maximized_window: Option<Window>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[getset(get_copy = "pub", set = "pub")]
    maximized_window_restore_idx: Option<usize>,
    #[getset(get = "pub", get_mut = "pub")]
    floating_windows: Vec<Window>,
    #[getset(get = "pub", get_mut = "pub", set = "pub")]
    layout: Layout,
    #[getset(get = "pub", get_mut = "pub", set = "pub")]
    layout_rules: Vec<(usize, Layout)>,
    #[getset(get_copy = "pub", set = "pub")]
    layout_flip: Option<Axis>,
    #[getset(get_copy = "pub", set = "pub")]
    workspace_padding: Option<i32>,
    #[getset(get_copy = "pub", set = "pub")]
    container_padding: Option<i32>,
    #[getset(get = "pub", set = "pub")]
    latest_layout: Vec<Rect>,
    #[getset(get = "pub", get_mut = "pub", set = "pub")]
    resize_dimensions: Vec<Option<Rect>>,
    #[getset(get = "pub", set = "pub")]
    tile: bool,
    pub is_poker_workspace: bool,
}

impl_ring_elements!(Workspace, Container);

impl Default for Workspace {
    fn default() -> Self {
        Self {
            name: None,
            containers: Ring::default(),
            monocle_container: None,
            maximized_window: None,
            maximized_window_restore_idx: None,
            monocle_container_restore_idx: None,
            floating_windows: Vec::default(),
            layout: Layout::Default(DefaultLayout::BSP),
            layout_rules: vec![],
            layout_flip: None,
            workspace_padding: Option::from(DEFAULT_WORKSPACE_PADDING.load(Ordering::SeqCst)),
            container_padding: Option::from(DEFAULT_CONTAINER_PADDING.load(Ordering::SeqCst)),
            latest_layout: vec![],
            resize_dimensions: vec![],
            tile: true,
            is_poker_workspace: false,
        }
    }
}

impl Workspace {
    pub fn load_static_config(&mut self, config: &WorkspaceConfig) -> Result<()> {
        self.name = Option::from(config.name.clone());
        self.is_poker_workspace = config.name == "POKER";

        if config.container_padding.is_some() {
            self.set_container_padding(config.container_padding);
        }

        if config.workspace_padding.is_some() {
            self.set_workspace_padding(config.workspace_padding);
        }

        if let Some(layout) = &config.layout {
            self.layout = Layout::Default(*layout);
            self.tile = true;
        }

        if let Some(pathbuf) = &config.custom_layout {
            let layout = CustomLayout::from_path(pathbuf)?;
            self.layout = Layout::Custom(layout);
            self.tile = true;
        }

        if config.custom_layout.is_none() && config.layout.is_none() {
            self.tile = false;
        }

        if let Some(layout_rules) = &config.layout_rules {
            let mut all_rules = vec![];
            for (count, rule) in layout_rules {
                all_rules.push((*count, Layout::Default(*rule)));
            }

            self.set_layout_rules(all_rules);
        }

        if let Some(layout_rules) = &config.custom_layout_rules {
            let rules = self.layout_rules_mut();
            for (count, pathbuf) in layout_rules {
                let rule = CustomLayout::from_path(pathbuf)?;
                rules.push((*count, Layout::Custom(rule)));
            }
        }

        Ok(())
    }

    pub fn hide(&mut self) {
        for container in self.containers_mut() {
            for window in container.windows_mut() {
                window.hide();
            }
        }

        if let Some(window) = self.maximized_window() {
            window.hide();
        }

        if let Some(container) = self.monocle_container_mut() {
            for window in container.windows_mut() {
                window.hide();
            }
        }

        for window in self.floating_windows() {
            window.hide();
        }
    }

    pub fn restore(&mut self, mouse_follows_focus: bool) -> Result<()> {
        let idx = self.focused_container_idx();
        let mut to_focus = None;
        for (i, container) in self.containers_mut().iter_mut().enumerate() {
            if let Some(window) = container.focused_window_mut() {
                window.restore();

                if idx == i {
                    to_focus = Option::from(*window);
                }
            }
        }

        if let Some(window) = self.maximized_window() {
            window.maximize();
        }

        if let Some(container) = self.monocle_container_mut() {
            for window in container.windows_mut() {
                window.restore();
            }
        }

        for window in self.floating_windows() {
            window.restore();
        }

        // Do this here to make sure that an error doesn't stop the restoration of other windows
        // Maximised windows should always be drawn at the top of the Z order
        if let Some(window) = to_focus {
            if self.maximized_window().is_none() {
                window.focus(mouse_follows_focus)?;
            }
        }

        Ok(())
    }

    pub fn update(&mut self, _work_area: &Rect, _offset: Option<Rect>) -> Result<()> {
        if !self.is_poker_workspace {
            return Ok(());
        }

        let poker_config = PokerConfig::default();
        let mut current_row = 0;
        let active_window_border_width = 5;
        let mut left = poker_config.at_left;
        let mut top = poker_config.at_top;

        let mut windows = vec![];
        for container in self.containers_mut() {
            windows.push(container.focused_window_mut());
        }
        let windows_count = windows.len();

        for (i, window) in windows.into_iter().enumerate() {
            if let Some(window) = window {
                for poker_window in &poker_config.windows {
                    let regex_identifiers = REGEX_IDENTIFIERS.lock();
                    let title = window.title().unwrap();
                    let exe_name = window.exe().unwrap();
                    let class = window.class().unwrap();
                    let path = window.path().unwrap();

                    let should_act = should_act(
                        &title,
                        &exe_name,
                        &class,
                        &path,
                        &poker_window.identifiers,
                        &regex_identifiers,
                    );

                    if should_act {
                        tracing::info!("POKER WINDOW: {} {}", i, title);

                        let row;

                        if windows_count <= 6 {
                            row = match i {
                                0 => 1,
                                1 => 1,
                                2 => 2,
                                3 => 2,
                                4 => 3,
                                5 => 3,
                                6 => 4,
                                7 => 4,
                                _ => 1,
                            };
                        } else {
                            row = match i {
                                0 => 1,
                                1 => 1,
                                2 => 1,
                                3 => 2,
                                4 => 2,
                                5 => 2,
                                6 => 3,
                                7 => 3,
                                8 => 3,
                                _ => 1,
                            };
                        }

                        let width = poker_window.width;
                        let height = poker_window.height;

                        if current_row != row {
                            left = poker_config.at_left;
                            top = poker_config.at_top + (height + active_window_border_width) * (row - 1);
                            current_row = row;

                            if (windows_count == 3 && row == 2) || (windows_count == 5 && row == 3) {
                                left += 325
                            }

                            if row == 3 {
                                top -= 50
                            }
                        }

                        let mut rect: Rect = Rect::default();
                        rect.left = left;
                        rect.top = top + active_window_border_width;
                        rect.right = width;
                        rect.bottom = height;

                        window.set_position(&rect, true)?;

                        left = rect.left + width + active_window_border_width;
                    }
                }
            }
        }

        Ok(())
    }

    pub fn reap_orphans(&mut self) -> Result<(usize, usize)> {
        let mut hwnds = vec![];
        let mut floating_hwnds = vec![];

        for window in self.visible_windows_mut().into_iter().flatten() {
            if !window.is_window() {
                hwnds.push(window.hwnd);
            }
        }

        for window in self.floating_windows() {
            if !window.is_window() {
                floating_hwnds.push(window.hwnd);
            }
        }

        for hwnd in &hwnds {
            tracing::debug!("reaping hwnd: {}", hwnd);
            self.remove_window(*hwnd)?;
        }

        for hwnd in &floating_hwnds {
            tracing::debug!("reaping floating hwnd: {}", hwnd);
            self.floating_windows_mut()
                .retain(|w| !floating_hwnds.contains(&w.hwnd));
        }

        let mut container_ids = vec![];
        for container in self.containers() {
            if container.windows().is_empty() {
                container_ids.push(container.id().clone());
            }
        }

        self.containers_mut()
            .retain(|c| !container_ids.contains(c.id()));

        Ok((hwnds.len() + floating_hwnds.len(), container_ids.len()))
    }

    pub fn container_for_window(&self, hwnd: isize) -> Option<&Container> {
        self.containers().get(self.container_idx_for_window(hwnd)?)
    }

    pub fn focus_container_by_window(&mut self, hwnd: isize) -> Result<()> {
        let container_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_else(|| anyhow!("there is no container/window"))?;

        let container = self
            .containers_mut()
            .get_mut(container_idx)
            .ok_or_else(|| anyhow!("there is no container"))?;

        let window_idx = container
            .idx_for_window(hwnd)
            .ok_or_else(|| anyhow!("there is no window"))?;

        container.focus_window(window_idx);
        self.focus_container(container_idx);

        Ok(())
    }

    pub fn container_idx_from_current_point(&self) -> Option<usize> {
        let mut idx = None;

        let point = WindowsApi::cursor_pos().ok()?;

        for (i, _container) in self.containers().iter().enumerate() {
            if let Some(rect) = self.latest_layout().get(i) {
                if rect.contains_point((point.x, point.y)) {
                    idx = Option::from(i);
                }
            }
        }

        idx
    }

    pub fn hwnd_from_exe(&self, exe: &str) -> Option<isize> {
        for container in self.containers() {
            if let Some(hwnd) = container.hwnd_from_exe(exe) {
                return Option::from(hwnd);
            }
        }

        if let Some(window) = self.maximized_window() {
            if let Ok(window_exe) = window.exe() {
                if exe == window_exe {
                    return Option::from(window.hwnd);
                }
            }
        }

        if let Some(container) = self.monocle_container() {
            if let Some(hwnd) = container.hwnd_from_exe(exe) {
                return Option::from(hwnd);
            }
        }

        for window in self.floating_windows() {
            if let Ok(window_exe) = window.exe() {
                if exe == window_exe {
                    return Option::from(window.hwnd);
                }
            }
        }

        None
    }

    pub fn contains_managed_window(&self, hwnd: isize) -> bool {
        for container in self.containers() {
            if container.contains_window(hwnd) {
                return true;
            }
        }

        if let Some(window) = self.maximized_window() {
            if hwnd == window.hwnd {
                return true;
            }
        }

        if let Some(container) = self.monocle_container() {
            if container.contains_window(hwnd) {
                return true;
            }
        }

        false
    }

    pub fn is_focused_window_monocle_or_maximized(&self) -> Result<bool> {
        let hwnd = WindowsApi::foreground_window()?;
        if let Some(window) = self.maximized_window() {
            if hwnd == window.hwnd {
                return Ok(true);
            }
        }

        if let Some(container) = self.monocle_container() {
            if container.contains_window(hwnd) {
                return Ok(true);
            }
        }

        Ok(false)
    }

    pub fn contains_window(&self, hwnd: isize) -> bool {
        for container in self.containers() {
            if container.contains_window(hwnd) {
                return true;
            }
        }

        if let Some(window) = self.maximized_window() {
            if hwnd == window.hwnd {
                return true;
            }
        }

        if let Some(container) = self.monocle_container() {
            if container.contains_window(hwnd) {
                return true;
            }
        }

        for window in self.floating_windows() {
            if hwnd == window.hwnd {
                return true;
            }
        }

        false
    }

    pub fn promote_container(&mut self) -> Result<()> {
        let resize = self.resize_dimensions_mut().remove(0);
        let container = self
            .remove_focused_container()
            .ok_or_else(|| anyhow!("there is no container"))?;

        let primary_idx = match self.layout() {
            Layout::Default(_) => 0,
            Layout::Custom(layout) => layout.first_container_idx(
                layout
                    .primary_idx()
                    .ok_or_else(|| anyhow!("this custom layout does not have a primary column"))?,
            ),
        };

        self.containers_mut().insert(primary_idx, container);
        self.resize_dimensions_mut().insert(primary_idx, resize);

        self.focus_container(primary_idx);

        Ok(())
    }

    pub fn add_container(&mut self, container: Container) {
        self.containers_mut().push_back(container);
        self.focus_last_container();
    }

    pub fn insert_container_at_idx(&mut self, idx: usize, container: Container) {
        self.containers_mut().insert(idx, container);
    }

    pub fn remove_container_by_idx(&mut self, idx: usize) -> Option<Container> {
        if idx < self.resize_dimensions().len() {
            self.resize_dimensions_mut().remove(idx);
        }

        if idx < self.containers().len() {
            return self.containers_mut().remove(idx);
        }

        None
    }

    fn container_idx_for_window(&self, hwnd: isize) -> Option<usize> {
        let mut idx = None;
        for (i, x) in self.containers().iter().enumerate() {
            if x.contains_window(hwnd) {
                idx = Option::from(i);
            }
        }

        idx
    }

    pub fn remove_window(&mut self, hwnd: isize) -> Result<()> {
        if self.floating_windows().iter().any(|w| w.hwnd == hwnd) {
            self.floating_windows_mut().retain(|w| w.hwnd != hwnd);
            return Ok(());
        }

        if let Some(container) = self.monocle_container_mut() {
            if let Some(window_idx) = container
                .windows()
                .iter()
                .position(|window| window.hwnd == hwnd)
            {
                container
                    .remove_window_by_idx(window_idx)
                    .ok_or_else(|| anyhow!("there is no window"))?;

                if container.windows().is_empty() {
                    self.set_monocle_container(None);
                    self.set_monocle_container_restore_idx(None);
                }

                return Ok(());
            }
        }

        if let Some(window) = self.maximized_window() {
            if window.hwnd == hwnd {
                window.unmaximize();
                self.set_maximized_window(None);
                self.set_maximized_window_restore_idx(None);
                return Ok(());
            }
        }

        let container_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_else(|| anyhow!("there is no window"))?;

        let container = self
            .containers_mut()
            .get_mut(container_idx)
            .ok_or_else(|| anyhow!("there is no container"))?;

        let window_idx = container
            .windows()
            .iter()
            .position(|window| window.hwnd == hwnd)
            .ok_or_else(|| anyhow!("there is no window"))?;

        container
            .remove_window_by_idx(window_idx)
            .ok_or_else(|| anyhow!("there is no window"))?;

        if container.windows().is_empty() {
            self.containers_mut()
                .remove(container_idx)
                .ok_or_else(|| anyhow!("there is no container"))?;

            // Whenever a container is empty, we need to remove any resize dimensions for it too
            if self.resize_dimensions().get(container_idx).is_some() {
                self.resize_dimensions_mut().remove(container_idx);
            }

            self.focus_previous_container();
        } else {
            container.load_focused_window();
        }

        Ok(())
    }

    pub fn remove_focused_container(&mut self) -> Option<Container> {
        let focused_idx = self.focused_container_idx();
        let container = self.remove_container_by_idx(focused_idx);
        self.focus_previous_container();

        container
    }

    pub fn remove_container(&mut self, idx: usize) -> Option<Container> {
        let container = self.remove_container_by_idx(idx);
        self.focus_previous_container();

        container
    }

    pub fn new_idx_for_direction(&self, direction: OperationDirection) -> Option<usize> {
        let len = NonZeroUsize::new(self.containers().len())?;

        direction.destination(
            self.layout().as_boxed_direction().as_ref(),
            self.layout_flip(),
            self.focused_container_idx(),
            len,
        )
    }
    pub fn new_idx_for_cycle_direction(&self, direction: CycleDirection) -> Option<usize> {
        Option::from(direction.next_idx(
            self.focused_container_idx(),
            NonZeroUsize::new(self.containers().len())?,
        ))
    }

    pub fn move_window_to_container(&mut self, target_container_idx: usize) -> Result<()> {
        let focused_idx = self.focused_container_idx();

        let container = self
            .focused_container_mut()
            .ok_or_else(|| anyhow!("there is no container"))?;

        let window = container
            .remove_focused_window()
            .ok_or_else(|| anyhow!("there is no window"))?;

        // This is a little messy
        let adjusted_target_container_index = if container.windows().is_empty() {
            self.containers_mut().remove(focused_idx);
            self.resize_dimensions_mut().remove(focused_idx);

            if focused_idx < target_container_idx {
                target_container_idx - 1
            } else {
                target_container_idx
            }
        } else {
            container.load_focused_window();
            target_container_idx
        };

        let target_container = self
            .containers_mut()
            .get_mut(adjusted_target_container_index)
            .ok_or_else(|| anyhow!("there is no container"))?;

        target_container.add_window(window);

        self.focus_container(adjusted_target_container_index);
        self.focused_container_mut()
            .ok_or_else(|| anyhow!("there is no container"))?
            .load_focused_window();

        Ok(())
    }

    pub fn new_container_for_focused_window(&mut self) -> Result<()> {
        let focused_container_idx = self.focused_container_idx();

        let container = self
            .focused_container_mut()
            .ok_or_else(|| anyhow!("there is no container"))?;

        let window = container
            .remove_focused_window()
            .ok_or_else(|| anyhow!("there is no window"))?;

        if container.windows().is_empty() {
            self.containers_mut().remove(focused_container_idx);
            self.resize_dimensions_mut().remove(focused_container_idx);
        } else {
            container.load_focused_window();
        }

        self.new_container_for_window(window);

        let mut container = Container::default();
        container.add_window(window);
        Ok(())
    }

    pub fn new_container_for_floating_window(&mut self) -> Result<()> {
        let focused_idx = self.focused_container_idx();
        let window = self
            .remove_focused_floating_window()
            .ok_or_else(|| anyhow!("there is no floating window"))?;

        let mut container = Container::default();
        container.add_window(window);
        self.containers_mut().insert(focused_idx, container);
        self.resize_dimensions_mut().insert(focused_idx, None);

        Ok(())
    }

    pub fn new_container_for_window(&mut self, window: Window) {
        let next_idx = if self.containers().is_empty() {
            0
        } else {
            self.focused_container_idx() + 1
        };

        let mut container = Container::default();
        container.add_window(window);

        if next_idx > self.containers().len() {
            self.containers_mut().push_back(container);
        } else {
            self.containers_mut().insert(next_idx, container);
        }

        if next_idx > self.resize_dimensions().len() {
            self.resize_dimensions_mut().push(None);
        } else {
            self.resize_dimensions_mut().insert(next_idx, None);
        }

        self.focus_container(next_idx);
    }

    pub fn new_floating_window(&mut self) -> Result<()> {
        let window = if let Some(maximized_window) = self.maximized_window() {
            let window = *maximized_window;
            self.set_maximized_window(None);
            self.set_maximized_window_restore_idx(None);
            window
        } else if let Some(monocle_container) = self.monocle_container_mut() {
            let window = monocle_container
                .remove_focused_window()
                .ok_or_else(|| anyhow!("there is no window"))?;

            if monocle_container.windows().is_empty() {
                self.set_monocle_container(None);
                self.set_monocle_container_restore_idx(None);
            } else {
                monocle_container.load_focused_window();
            }

            window
        } else {
            let focused_idx = self.focused_container_idx();

            let container = self
                .focused_container_mut()
                .ok_or_else(|| anyhow!("there is no container"))?;

            let window = container
                .remove_focused_window()
                .ok_or_else(|| anyhow!("there is no window"))?;

            if container.windows().is_empty() {
                self.containers_mut().remove(focused_idx);
                self.resize_dimensions_mut().remove(focused_idx);
            } else {
                container.load_focused_window();
            }

            window
        };

        self.floating_windows_mut().push(window);

        Ok(())
    }

    pub fn new_monocle_container(&mut self) -> Result<()> {
        let focused_idx = self.focused_container_idx();
        let container = self
            .containers_mut()
            .remove(focused_idx)
            .ok_or_else(|| anyhow!("there is no container"))?;

        // We don't remove any resize adjustments for a monocle, because when this container is
        // inevitably reintegrated, it would be weird if it doesn't go back to the dimensions
        // it had before

        self.set_monocle_container(Option::from(container));
        self.set_monocle_container_restore_idx(Option::from(focused_idx));
        self.focus_previous_container();

        self.monocle_container_mut()
            .as_mut()
            .ok_or_else(|| anyhow!("there is no monocle container"))?
            .load_focused_window();

        Ok(())
    }

    pub fn reintegrate_monocle_container(&mut self) -> Result<()> {
        let restore_idx = self
            .monocle_container_restore_idx()
            .ok_or_else(|| anyhow!("there is no monocle restore index"))?;

        let container = self
            .monocle_container_mut()
            .as_ref()
            .ok_or_else(|| anyhow!("there is no monocle container"))?;

        let container = container.clone();
        if restore_idx > self.containers().len() - 1 {
            self.containers_mut()
                .resize(restore_idx, Container::default());
        }

        self.containers_mut().insert(restore_idx, container);
        self.focus_container(restore_idx);
        self.focused_container_mut()
            .ok_or_else(|| anyhow!("there is no container"))?
            .load_focused_window();

        self.set_monocle_container(None);
        self.set_monocle_container_restore_idx(None);

        Ok(())
    }

    pub fn new_maximized_window(&mut self) -> Result<()> {
        let focused_idx = self.focused_container_idx();
        let foreground_hwnd = WindowsApi::foreground_window()?;
        let mut floating_window = None;

        if !self.floating_windows().is_empty() {
            let mut focused_floating_window_idx = None;
            for (i, w) in self.floating_windows().iter().enumerate() {
                if w.hwnd == foreground_hwnd {
                    focused_floating_window_idx = Option::from(i);
                }
            }

            if let Some(idx) = focused_floating_window_idx {
                floating_window = Option::from(self.floating_windows_mut().remove(idx));
            }
        }

        if let Some(floating_window) = floating_window {
            self.set_maximized_window(Option::from(floating_window));
            self.set_maximized_window_restore_idx(Option::from(focused_idx));
            if let Some(window) = self.maximized_window() {
                window.maximize();
            }

            return Ok(());
        }

        let monocle_restore_idx = self.monocle_container_restore_idx();
        if let Some(monocle_container) = self.monocle_container_mut() {
            let window = monocle_container
                .remove_focused_window()
                .ok_or_else(|| anyhow!("there is no window"))?;

            if monocle_container.windows().is_empty() {
                self.set_monocle_container(None);
                self.set_monocle_container_restore_idx(None);
            } else {
                monocle_container.load_focused_window();
            }

            self.set_maximized_window(Option::from(window));
            self.set_maximized_window_restore_idx(monocle_restore_idx);
            if let Some(window) = self.maximized_window() {
                window.maximize();
            }

            return Ok(());
        }

        let container = self
            .focused_container_mut()
            .ok_or_else(|| anyhow!("there is no container"))?;

        let window = container
            .remove_focused_window()
            .ok_or_else(|| anyhow!("there is no window"))?;

        if container.windows().is_empty() {
            self.containers_mut().remove(focused_idx);
            if self.resize_dimensions().get(focused_idx).is_some() {
                self.resize_dimensions_mut().remove(focused_idx);
            }
        } else {
            container.load_focused_window();
        }

        self.set_maximized_window(Option::from(window));
        self.set_maximized_window_restore_idx(Option::from(focused_idx));

        if let Some(window) = self.maximized_window() {
            window.maximize();
        }

        self.focus_previous_container();

        Ok(())
    }

    pub fn reintegrate_maximized_window(&mut self) -> Result<()> {
        let restore_idx = self
            .maximized_window_restore_idx()
            .ok_or_else(|| anyhow!("there is no monocle restore index"))?;

        let window = self
            .maximized_window()
            .as_ref()
            .ok_or_else(|| anyhow!("there is no monocle container"))?;

        let window = *window;
        if !self.containers().is_empty() && restore_idx > self.containers().len() - 1 {
            self.containers_mut()
                .resize(restore_idx, Container::default());
        }

        let mut container = Container::default();
        container.windows_mut().push_back(window);
        self.containers_mut().insert(restore_idx, container);

        self.focus_container(restore_idx);

        self.focused_container_mut()
            .ok_or_else(|| anyhow!("there is no container"))?
            .load_focused_window();

        self.set_maximized_window(None);
        self.set_maximized_window_restore_idx(None);

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn focus_container(&mut self, idx: usize) {
        tracing::info!("focusing container");

        self.containers.focus(idx);
    }

    pub fn swap_containers(&mut self, i: usize, j: usize) {
        self.containers.swap(i, j);
        self.focus_container(j);
    }

    pub fn remove_focused_floating_window(&mut self) -> Option<Window> {
        let hwnd = WindowsApi::foreground_window().ok()?;

        let mut idx = None;
        for (i, window) in self.floating_windows.iter().enumerate() {
            if hwnd == window.hwnd {
                idx = Option::from(i);
            }
        }

        match idx {
            None => None,
            Some(idx) => {
                if self.floating_windows.get(idx).is_some() {
                    Option::from(self.floating_windows_mut().remove(idx))
                } else {
                    None
                }
            }
        }
    }

    pub fn visible_windows(&self) -> Vec<Option<&Window>> {
        let mut vec = vec![];
        for container in self.containers() {
            vec.push(container.focused_window());
        }

        vec
    }

    pub fn visible_window_details(&self) -> Vec<WindowDetails> {
        let mut vec: Vec<WindowDetails> = vec![];

        for container in self.containers() {
            if let Some(focused) = container.focused_window() {
                if let Ok(details) = (*focused).try_into() {
                    vec.push(details);
                }
            }
        }

        vec
    }

    pub fn visible_windows_mut(&mut self) -> Vec<Option<&mut Window>> {
        let mut vec = vec![];
        for container in self.containers_mut() {
            vec.push(container.focused_window_mut());
        }

        vec
    }

    fn focus_previous_container(&mut self) {
        let focused_idx = self.focused_container_idx();

        if focused_idx != 0 {
            self.focus_container(focused_idx - 1);
        }
    }

    fn focus_last_container(&mut self) {
        self.focus_container(self.containers().len() - 1);
    }
}
