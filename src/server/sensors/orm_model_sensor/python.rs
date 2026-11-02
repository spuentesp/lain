use super::django::{DjangoModelMap, TableResolution};
use super::{add_column, add_model, OrmAnalysis};
use crate::schema::StaticSource;

pub(super) fn add_django(analysis: &mut OrmAnalysis, map: &DjangoModelMap) {
    for (index, model) in map.models.iter().enumerate() {
        if !map.is_model(index) {
            continue;
        }
        match map.table_of(index) {
            TableResolution::Table(table) => {
                add_model(
                    analysis,
                    "django",
                    &model.name,
                    &table,
                    &model.path,
                    model.line,
                    StaticSource::TreeSitter,
                );
                for field in &model.fields {
                    if let Some(column) = &field.column {
                        add_column(
                            analysis,
                            &table,
                            column,
                            &model.path,
                            field.line,
                            StaticSource::TreeSitter,
                        );
                    } else {
                        analysis.unresolved.push(format!(
                            "{}:{}: dynamic column {} on {}",
                            model.path, field.line, field.attribute, model.name
                        ));
                    }
                }
            }
            TableResolution::Abstract => {}
            TableResolution::Unresolved(why) => {
                analysis.unresolved.push(format!(
                    "{}:{}: {why} on {}",
                    model.path, model.line, model.name
                ));
            }
        }
    }
}
