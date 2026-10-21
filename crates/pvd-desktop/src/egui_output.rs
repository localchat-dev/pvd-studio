use bevy::{asset::AssetEventSystems, prelude::*};
use bevy_egui::EguiPostUpdateSet;

pub struct Plugin;

impl bevy::prelude::Plugin for Plugin {
    fn build(&self, app: &mut App) {
        // Render extraction needs the atlas asset event from the same pass as the glyph UVs.
        app.configure_sets(
            PostUpdate,
            EguiPostUpdateSet::PostProcessOutput.before(AssetEventSystems),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::{AssetApp, AssetPlugin};
    use bevy::ecs::message::MessageCursor;
    use bevy_egui::{EguiManagedTextures, EguiRenderOutput, update_egui_textures_system};

    struct Atlas {
        app: App,
        entity: Entity,
        cursor: MessageCursor<AssetEvent<Image>>,
    }

    impl Atlas {
        fn new(ordered: bool) -> Self {
            let mut app = App::new();
            app.add_plugins((MinimalPlugins, AssetPlugin::default()))
                .init_asset::<Image>()
                .init_resource::<EguiManagedTextures>()
                .add_systems(
                    PostUpdate,
                    update_egui_textures_system.in_set(EguiPostUpdateSet::PostProcessOutput),
                );
            if ordered {
                app.add_plugins(Plugin);
            } else {
                app.configure_sets(
                    PostUpdate,
                    AssetEventSystems.before(EguiPostUpdateSet::PostProcessOutput),
                );
            }
            let entity = app.world_mut().spawn(EguiRenderOutput::default()).id();
            Self {
                app,
                entity,
                cursor: MessageCursor::default(),
            }
        }

        fn update(&mut self, delta: egui::epaint::ImageDelta) -> AssetId<Image> {
            self.app
                .world_mut()
                .get_mut::<EguiRenderOutput>(self.entity)
                .unwrap()
                .textures_delta
                .set = vec![(egui::TextureId::Managed(0), delta)];
            self.app.update();
            self.app.world().resource::<EguiManagedTextures>()[&(self.entity, 0)]
                .handle
                .id()
        }

        fn full(&mut self, width: usize) -> AssetId<Image> {
            self.update(egui::epaint::ImageDelta::full(
                egui::ColorImage::filled([width, 1], egui::Color32::BLACK),
                egui::TextureOptions::LINEAR,
            ))
        }

        fn partial(&mut self) -> AssetId<Image> {
            self.update(egui::epaint::ImageDelta::partial(
                [1, 0],
                egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
                egui::TextureOptions::LINEAR,
            ))
        }

        fn events(&mut self) -> Vec<AssetEvent<Image>> {
            self.cursor
                .read(self.app.world().resource::<Messages<AssetEvent<Image>>>())
                .copied()
                .collect()
        }

        fn idle(&mut self) {
            self.app
                .world_mut()
                .get_mut::<EguiRenderOutput>(self.entity)
                .unwrap()
                .textures_delta = Default::default();
            self.app.update();
        }

        fn pixel(&self, id: AssetId<Image>, x: u32) -> f32 {
            self.app
                .world()
                .resource::<Assets<Image>>()
                .get(id)
                .unwrap()
                .get_color_at(x, 0)
                .unwrap()
                .to_srgba()
                .red
        }
    }

    #[test]
    fn an_atlas_updated_after_asset_publication_reaches_extraction_one_tick_late() {
        let mut atlas = Atlas::new(false);
        let id = atlas.full(2);
        assert!(!atlas.events().contains(&AssetEvent::Added { id }));
        atlas.idle();
        assert!(atlas.events().contains(&AssetEvent::Added { id }));
        assert_eq!(atlas.partial(), id);
        assert_eq!(atlas.pixel(id, 1), 1.0);
        assert!(!atlas.events().contains(&AssetEvent::Modified { id }));
        atlas.idle();
        assert!(atlas.events().contains(&AssetEvent::Modified { id }));
    }

    #[test]
    fn full_and_partial_atlas_changes_publish_with_the_current_ui_pass() {
        let mut atlas = Atlas::new(true);
        let id = atlas.full(2);
        assert!(atlas.events().contains(&AssetEvent::Added { id }));
        assert_eq!(atlas.pixel(id, 1), 0.0);
        assert_eq!(atlas.partial(), id);
        assert_eq!(atlas.pixel(id, 1), 1.0);
        assert!(atlas.events().contains(&AssetEvent::Modified { id }));
        atlas.idle();
        assert!(!atlas.events().contains(&AssetEvent::Modified { id }));
    }

    #[test]
    fn replacing_an_atlas_publishes_the_new_handle_before_extraction() {
        let mut atlas = Atlas::new(true);
        let old = atlas.full(2);
        let new = atlas.full(4);
        assert_ne!(old, new);
        assert!(atlas.events().contains(&AssetEvent::Added { id: new }));
        assert_eq!(atlas.pixel(new, 3), 0.0);
    }
}
