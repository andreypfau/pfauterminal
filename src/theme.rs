use crate::colors::ColorScheme;

#[derive(Clone)]
pub struct Theme {
    pub colors: ColorScheme,
    pub tab_bar: TabBarTheme,
    pub dropdown: DropdownTheme,
    pub dialog: DialogTheme,
    pub panel: PanelTheme,
    pub general: GeneralTheme,
}

impl Theme {
    pub fn new() -> Self {
        Self {
            colors: ColorScheme::load(),
            tab_bar: TabBarTheme::default(),
            dropdown: DropdownTheme::default(),
            dialog: DialogTheme::default(),
            panel: PanelTheme::default(),
            general: GeneralTheme::default(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct GeneralTheme {
    pub panel_area_padding: f32,
}

impl Default for GeneralTheme {
    fn default() -> Self {
        Self {
            panel_area_padding: 8.0,
        }
    }
}

#[derive(Clone)]
pub struct TabBarTheme {
    pub height: f32,
    pub tab_padding_h: f32,
    pub tab_padding_v: f32,
    pub tab_gap: f32,
    pub icon_size: f32,
    pub icon_gap: f32,
    pub close_size: f32,
    pub border_width: f32,
    pub separator_height: f32,
    pub tab_radius: f32,
    pub plus_size: f32,
    pub plus_icon_size: f32,
    pub plus_radius: f32,
    pub font_size: f32,
}

impl Default for TabBarTheme {
    fn default() -> Self {
        Self {
            height: 36.0,
            tab_padding_h: 12.0,
            tab_padding_v: 5.0,
            tab_gap: 4.0,
            icon_size: 13.0,
            icon_gap: 6.0,
            close_size: 11.0,
            border_width: 1.0,
            separator_height: 1.0,
            tab_radius: 6.0,
            plus_size: 22.0,
            plus_icon_size: 12.0,
            plus_radius: 5.0,
            font_size: 11.0,
        }
    }
}

#[derive(Clone)]
pub struct DropdownTheme {
    pub width: f32,
    pub corner_radius: f32,
    pub padding: f32,
    pub item_height: f32,
    pub item_padding_h: f32,
    pub item_radius: f32,
    pub border_width: f32,
    pub font_size: f32,
    pub anchor_gap: f32,
    pub shadow_spread: f32,
    pub shadow_offset_y: f32,
    pub separator_height: f32,
    pub icon_size: f32,
    pub icon_gap: f32,
    pub close_size: f32,
}

impl Default for DropdownTheme {
    fn default() -> Self {
        Self {
            width: 200.0,
            corner_radius: 8.0,
            padding: 6.0,
            item_height: 32.0,
            item_padding_h: 12.0,
            item_radius: 6.0,
            border_width: 1.0,
            font_size: 13.0,
            anchor_gap: 4.0,
            shadow_spread: 20.0,
            shadow_offset_y: 4.0,
            separator_height: 1.0,
            icon_size: 13.0,
            icon_gap: 8.0,
            close_size: 11.0,
        }
    }
}

#[derive(Clone)]
pub struct DialogTheme {
    pub width: f32,
    pub border_width: f32,
    pub title_bar_height: f32,
    pub form_pad_v: f32,
    pub form_pad_h: f32,
    pub form_row_gap: f32,
    pub label_width: f32,
    pub field_height: f32,
    pub field_pad_h: f32,
    pub field_radius: f32,
    pub field_gap: f32,
    pub port_field_width: f32,
    pub port_spacer_width: f32,
    pub browse_btn_size: f32,
    pub footer_pad_v: f32,
    pub footer_pad_h: f32,
    pub footer_gap: f32,
    pub button_radius: f32,
    pub cancel_pad_h: f32,
    pub cancel_pad_v: f32,
    pub ok_pad_h: f32,
    pub font_size: f32,
    pub max_rounded_rects: usize,
}

impl Default for DialogTheme {
    fn default() -> Self {
        Self {
            width: 620.0,
            border_width: 1.0,
            title_bar_height: 40.0,
            form_pad_v: 20.0,
            form_pad_h: 28.0,
            form_row_gap: 24.0,
            label_width: 160.0,
            field_height: 36.0,
            field_pad_h: 10.0,
            field_radius: 4.0,
            field_gap: 16.0,
            port_field_width: 56.0,
            port_spacer_width: 112.0,
            browse_btn_size: 36.0,
            footer_pad_v: 20.0,
            footer_pad_h: 28.0,
            footer_gap: 12.0,
            button_radius: 6.0,
            cancel_pad_h: 20.0,
            cancel_pad_v: 8.0,
            ok_pad_h: 28.0,
            font_size: 13.0,
            max_rounded_rects: 80,
        }
    }
}

#[derive(Clone)]
pub struct PanelTheme {
    pub island_padding: f32,
    pub island_radius: f32,
    pub island_stroke_width: f32,
}

impl Default for PanelTheme {
    fn default() -> Self {
        Self {
            island_padding: 16.0,
            island_radius: 10.0,
            island_stroke_width: 0.5,
        }
    }
}
