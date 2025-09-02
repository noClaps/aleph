mod copy_path_tool;
mod create_directory_tool;
mod delete_path_tool;
mod diagnostics_tool;
pub mod edit_agent;
mod edit_file_tool;
mod fetch_tool;
mod find_path_tool;
mod grep_tool;
mod list_directory_tool;
mod move_path_tool;
mod now_tool;
mod open_tool;
mod project_notifications_tool;
mod read_file_tool;
mod schema;
pub mod templates;
mod terminal_tool;
mod thinking_tool;
mod ui;
mod web_search_tool;

use assistant_tool::ToolRegistry;
use copy_path_tool::CopyPathTool;
use gpui::{App, Entity};
use http_client::HttpClientWithUrl;
use language_model::LanguageModelRegistry;
use move_path_tool::MovePathTool;
use std::sync::Arc;
use web_search_tool::WebSearchTool;

pub(crate) use templates::*;

use crate::create_directory_tool::CreateDirectoryTool;
use crate::delete_path_tool::DeletePathTool;
use crate::diagnostics_tool::DiagnosticsTool;
use crate::edit_file_tool::EditFileTool;
use crate::fetch_tool::FetchTool;
use crate::list_directory_tool::ListDirectoryTool;
use crate::now_tool::NowTool;
use crate::thinking_tool::ThinkingTool;

pub use edit_file_tool::{EditFileMode, EditFileToolInput};
pub use find_path_tool::*;
pub use grep_tool::{GrepTool, GrepToolInput};
pub use open_tool::OpenTool;
pub use project_notifications_tool::ProjectNotificationsTool;
pub use read_file_tool::{ReadFileTool, ReadFileToolInput};
pub use terminal_tool::TerminalTool;

pub fn init(http_client: Arc<HttpClientWithUrl>, cx: &mut App) {
    assistant_tool::init(cx);

    let registry = ToolRegistry::global(cx);
    registry.register_tool(TerminalTool::new(cx));
    registry.register_tool(CreateDirectoryTool);
    registry.register_tool(CopyPathTool);
    registry.register_tool(DeletePathTool);
    registry.register_tool(MovePathTool);
    registry.register_tool(DiagnosticsTool);
    registry.register_tool(ListDirectoryTool);
    registry.register_tool(NowTool);
    registry.register_tool(OpenTool);
    registry.register_tool(ProjectNotificationsTool);
    registry.register_tool(FindPathTool);
    registry.register_tool(ReadFileTool);
    registry.register_tool(GrepTool);
    registry.register_tool(ThinkingTool);
    registry.register_tool(FetchTool::new(http_client));
    registry.register_tool(EditFileTool);

    register_web_search_tool(&LanguageModelRegistry::global(cx), cx);
    cx.subscribe(
        &LanguageModelRegistry::global(cx),
        move |registry, event, cx| {
            if let language_model::Event::DefaultModelChanged = event {
                register_web_search_tool(&registry, cx);
            }
        },
    )
    .detach();
}

fn register_web_search_tool(registry: &Entity<LanguageModelRegistry>, cx: &mut App) {
    let using_zed_provider = registry
        .read(cx)
        .default_model()
        .is_some_and(|default| default.is_provided_by_zed());
    if using_zed_provider {
        ToolRegistry::global(cx).register_tool(WebSearchTool);
    } else {
        ToolRegistry::global(cx).unregister_tool(WebSearchTool);
    }
}
