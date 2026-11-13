//! Resolve URL identities without mistaking incomplete hydration for absence.

use super::super::{SurfaceData, project_work};
use super::RouteRefusal;
use crate::view::{AppRoute, TaskView, WorkLocation, WorkPlace};
use crate::work::{TaskOverviewSelection, WorkRouteResolution};
use yew::prelude::*;

#[hook]
pub(in crate::browser) fn use_selection(
    route: &AppRoute,
    surface: &SurfaceData,
    refuse: Callback<RouteRefusal>,
) -> Option<TaskOverviewSelection> {
    let mut refusal;
    let selected = {
        let surface = surface.read();
        let runtime = surface.as_ref();
        let work = project_work(runtime);
        refusal = route
            .without_missing_tasks(|task| {
                runtime
                    .and_then(|runtime| runtime.task_detail(task))
                    .and_then(crate::runtime::TaskDetailState::known)
                    == Some(false)
            })
            .map(|(route, text)| RouteRefusal { route, text });
        match route {
            AppRoute::Work(location) => match &location.place {
                WorkPlace::Scope(scope) => {
                    match crate::work::resolve_work_route(scope, work.as_ref()) {
                        WorkRouteResolution::Pending => None,
                        WorkRouteResolution::Missing => {
                            refusal = Some(RouteRefusal {
                                route: AppRoute::chat(),
                                text: format!(
                                    "Work scope `{}` is unavailable; opened Chat.",
                                    scope.path()
                                ),
                            });
                            None
                        }
                        WorkRouteResolution::Resolved(scope) => {
                            work.as_ref().map(|work| work.overview_for_scope(scope))
                        }
                    }
                }
                WorkPlace::Task { task, view } => {
                    let detail = runtime.and_then(|runtime| runtime.task_detail(task));
                    if let Some(answer) = detail.and_then(crate::runtime::TaskDetailState::answer) {
                        let design = answer.task.design_standing().is_some();
                        if matches!(view, Some(TaskView::Questions | TaskView::History)) && !design
                            || *view == Some(TaskView::Acceptance) && design
                        {
                            refusal = Some(RouteRefusal {
                                route: AppRoute::Work(WorkLocation {
                                    place: WorkPlace::Task {
                                        task: task.clone(),
                                        view: None,
                                    },
                                    options: location.options.clone(),
                                }),
                                text: "That task view is unavailable; opened its room.".into(),
                            });
                        }
                    }
                    Some(TaskOverviewSelection::task(task.clone()))
                }
                WorkPlace::Selection(selection) => Some(selection.to_overview()),
            },
            AppRoute::Chat { .. } | AppRoute::Spice => None,
        }
    };
    use_effect_with(refusal, move |refusal| {
        if let Some(refusal) = refusal {
            refuse.emit(refusal.clone());
        }
    });
    selected
}
