//! Line icons (`assets/icons/ui/*.svg`, white strokes) rendered by the egui_extras SVG
//! loader at the size they are shown, tinted to the theme colour.

use eframe::egui::{self, Color32};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Overview,
    Folder,
    FolderPlus,
    FolderOpen,
    User,
    UserPlus,
    Settings,
    Logs,
    Cloud,
    Play,
    Stop,
    Pause,
    Retry,
    Check,
    CheckCircle,
    X,
    XCircle,
    Warning,
    Info,
    Danger,
    Copy,
    Save,
    Login,
    Logout,
    External,
    Trash,
    Download,
    Sync,
    Search,
    Upload,
    Edit,
}

impl Icon {
    pub fn source(self) -> egui::ImageSource<'static> {
        match self {
            Self::Overview => egui::include_image!("../../assets/icons/ui/overview.svg"),
            Self::Folder => egui::include_image!("../../assets/icons/ui/folder.svg"),
            Self::FolderPlus => egui::include_image!("../../assets/icons/ui/folder-plus.svg"),
            Self::FolderOpen => egui::include_image!("../../assets/icons/ui/folder-open.svg"),
            Self::User => egui::include_image!("../../assets/icons/ui/user.svg"),
            Self::UserPlus => egui::include_image!("../../assets/icons/ui/user-plus.svg"),
            Self::Settings => egui::include_image!("../../assets/icons/ui/settings.svg"),
            Self::Logs => egui::include_image!("../../assets/icons/ui/logs.svg"),
            Self::Cloud => egui::include_image!("../../assets/icons/ui/cloud.svg"),
            Self::Play => egui::include_image!("../../assets/icons/ui/play.svg"),
            Self::Stop => egui::include_image!("../../assets/icons/ui/stop.svg"),
            Self::Pause => egui::include_image!("../../assets/icons/ui/pause.svg"),
            Self::Retry => egui::include_image!("../../assets/icons/ui/retry.svg"),
            Self::Check => egui::include_image!("../../assets/icons/ui/check.svg"),
            Self::CheckCircle => egui::include_image!("../../assets/icons/ui/check-circle.svg"),
            Self::X => egui::include_image!("../../assets/icons/ui/x.svg"),
            Self::XCircle => egui::include_image!("../../assets/icons/ui/x-circle.svg"),
            Self::Warning => egui::include_image!("../../assets/icons/ui/warning.svg"),
            Self::Info => egui::include_image!("../../assets/icons/ui/info.svg"),
            Self::Danger => egui::include_image!("../../assets/icons/ui/danger.svg"),
            Self::Copy => egui::include_image!("../../assets/icons/ui/copy.svg"),
            Self::Save => egui::include_image!("../../assets/icons/ui/save.svg"),
            Self::Login => egui::include_image!("../../assets/icons/ui/login.svg"),
            Self::Logout => egui::include_image!("../../assets/icons/ui/logout.svg"),
            Self::External => egui::include_image!("../../assets/icons/ui/external.svg"),
            Self::Trash => egui::include_image!("../../assets/icons/ui/trash.svg"),
            Self::Download => egui::include_image!("../../assets/icons/ui/download.svg"),
            Self::Sync => egui::include_image!("../../assets/icons/ui/sync.svg"),
            Self::Search => egui::include_image!("../../assets/icons/ui/search.svg"),
            Self::Upload => egui::include_image!("../../assets/icons/ui/upload.svg"),
            Self::Edit => egui::include_image!("../../assets/icons/ui/edit.svg"),
        }
    }

    /// The icon `size` points square, in `color`.
    pub fn image(self, size: f32, color: Color32) -> egui::Image<'static> {
        egui::Image::new(self.source())
            .fit_to_exact_size(egui::vec2(size, size))
            .tint(color)
    }

    /// Add it to the layout.
    pub fn show(self, ui: &mut egui::Ui, size: f32, color: Color32) -> egui::Response {
        ui.add(self.image(size, color))
    }

    /// Paint it centred in `rect` (custom-drawn widgets).
    pub fn paint(self, ui: &egui::Ui, rect: egui::Rect, color: Color32) {
        self.image(rect.width(), color).paint_at(ui, rect);
    }
}
