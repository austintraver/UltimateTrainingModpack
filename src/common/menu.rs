use std::collections::HashMap;
use std::fs;
use std::io::BufReader;

use skyline::nn::hid::GetNpadStyleSet;

use crate::common::{button_config, ButtonConfig};
use crate::common::{DEFAULTS_MENU, MENU};
use crate::events::{Event, EVENT_QUEUE};
use crate::input::{ButtonBitfield, ControllerStyle, MappedInputs, SomeControllerStruct};
use crate::logging::*;
use crate::training::frame_counter;

use training_mod_consts::{create_app, InputControl, MenuJsonStruct, MENU_OPTIONS_PATH};
use training_mod_sync::*;
use training_mod_tui::AppPage;

use DirectionButton::*;

pub const MENU_CLOSE_WAIT_FRAMES: u32 = 15;
pub static QUICK_MENU_ACTIVE: RwLock<bool> = RwLock::new(false);

pub unsafe fn menu_condition() -> bool {
    button_config::combo_passes(button_config::ButtonCombo::OpenMenu)
}

pub fn load_from_file() {
    // Note that this function requires a larger stack size
    // With the switch default, it'll crash w/o a helpful error message
    info!("Checking for previous menu in {MENU_OPTIONS_PATH}...");
    let err_msg = format!("Could not read {}", MENU_OPTIONS_PATH);
    if fs::metadata(MENU_OPTIONS_PATH).is_ok() {
        let menu_conf = fs::File::open(MENU_OPTIONS_PATH).expect(&err_msg);
        let reader = BufReader::new(menu_conf);
        if let Ok(menu_conf_json) = serde_json::from_reader::<BufReader<_>, MenuJsonStruct>(reader)
        {
            assign(&MENU, menu_conf_json.menu);
            assign(&DEFAULTS_MENU, menu_conf_json.defaults_menu);
            info!("Previous menu found. Loading...");
        } else {
            warn!("Previous menu found but is invalid. Deleting...");
            let err_msg = format!(
                "{} has invalid schema but could not be deleted!",
                MENU_OPTIONS_PATH
            );
            fs::remove_file(MENU_OPTIONS_PATH).expect(&err_msg);
        }
    } else {
        info!("No previous menu file found.");
    }
    info!("Setting initial menu selections...");
    let mut app = lock_write(&QUICK_MENU_APP);
    app.serialized_default_settings =
        serde_json::to_string(&read(&DEFAULTS_MENU)).expect("Could not serialize DEFAULTS_MENU");
    app.update_all_from_json(
        &serde_json::to_string(&read(&MENU)).expect("Could not serialize MENU"),
    );
}

pub fn set_menu_from_json(message: &str) {
    let response = serde_json::from_str::<MenuJsonStruct>(message);
    info!("Received menu message: {message}");
    if let Ok(message_json) = response {
        // Includes both MENU and DEFAULTS_MENU
        assign(&MENU, message_json.menu);
        assign(&DEFAULTS_MENU, message_json.defaults_menu);
        fs::write(
            MENU_OPTIONS_PATH,
            serde_json::to_string_pretty(&message_json).unwrap(),
        )
        .expect("Failed to write menu settings file");
    } else {
        skyline::error::show_error(
            0x70,
            "Could not parse the menu response!\nPlease send a screenshot of the details page to the developers.\n\0",
            &format!("{message:#?}\0"),
        );
    };
}

pub fn spawn_menu() {
    assign(&QUICK_MENU_ACTIVE, true);
    let mut app = lock_write(&QUICK_MENU_APP);
    app.page = AppPage::SUBMENU;
    assign(&MENU_RECEIVED_INPUT, true);
}

#[derive(Eq, PartialEq, Hash, Copy, Clone)]
enum DirectionButton {
    LLeft,
    RLeft,
    LDown,
    RDown,
    LRight,
    RRight,
    LUp,
    RUp,
}

pub static QUICK_MENU_APP: LazyLock<RwLock<training_mod_tui::App<'static>>> = LazyLock::new(|| {
    RwLock::new({
        info!("Initialized lazy_static: QUICK_MENU_APP");
        unsafe { create_app() }
    })
});
pub static P1_CONTROLLER_STYLE: LazyLock<RwLock<ControllerStyle>> =
    LazyLock::new(|| RwLock::new(ControllerStyle::default()));
static DIRECTION_HOLD_FRAMES: LazyLock<RwLock<HashMap<DirectionButton, u32>>> =
    LazyLock::new(|| {
        RwLock::new(HashMap::from([
            (LLeft, 0),
            (RLeft, 0),
            (LDown, 0),
            (RDown, 0),
            (LRight, 0),
            (RRight, 0),
            (LUp, 0),
            (RUp, 0),
        ]))
    });
pub static MENU_RECEIVED_INPUT: RwLock<bool> = RwLock::new(true);

pub static MENU_CLOSE_FRAME_COUNTER: LazyLock<usize> =
    LazyLock::new(|| frame_counter::register_counter(frame_counter::FrameCounterType::Real));

pub fn handle_final_input_mapping(
    player_idx: i32,
    controller_struct: &mut SomeControllerStruct,
    out: *mut MappedInputs,
) {
    unsafe {
        if player_idx == 0 {
            let p1_controller = &mut *controller_struct.controller;
            assign(&P1_CONTROLLER_STYLE, p1_controller.style);
            let visual_frame_count = frame_counter::get_frame_count(*MENU_CLOSE_FRAME_COUNTER);
            if visual_frame_count > 0 && visual_frame_count < MENU_CLOSE_WAIT_FRAMES {
                // If we just closed the menu, kill all inputs to avoid accidental presses
                *out = MappedInputs::empty();
                p1_controller.current_buttons = ButtonBitfield::default();
                p1_controller.previous_buttons = ButtonBitfield::default();
                p1_controller.just_down = ButtonBitfield::default();
                p1_controller.just_release = ButtonBitfield::default();
            } else if visual_frame_count >= MENU_CLOSE_WAIT_FRAMES {
                frame_counter::stop_counting(*MENU_CLOSE_FRAME_COUNTER);
                frame_counter::reset_frame_count(*MENU_CLOSE_FRAME_COUNTER);
            }

            if read(&QUICK_MENU_ACTIVE) {
                // If we're here, remove all other presses
                *out = MappedInputs::empty();

                let mut received_input = false;

                const DIRECTION_HOLD_REPEAT_FRAMES: u32 = 20;
                use DirectionButton::*;
                let mut direction_hold_frames = read_clone(&DIRECTION_HOLD_FRAMES); // TODO!("Refactor this, it doesn't need to be a hashmap")

                // Check for all controllers unplugged
                let mut potential_controller_ids = (0..8).collect::<Vec<u32>>();
                potential_controller_ids.push(0x20);
                if potential_controller_ids
                    .iter()
                    .all(|i| GetNpadStyleSet(i as *const _).flags == 0)
                {
                    assign(&QUICK_MENU_ACTIVE, false);
                    return;
                }

                let style = p1_controller.style;
                let button_presses = p1_controller.just_down;

                let button_current_held = p1_controller.current_buttons;
                direction_hold_frames
                    .iter_mut()
                    .for_each(|(direction, frames)| {
                        let still_held = match direction {
                            LLeft => button_current_held.l_left(),
                            RLeft => button_current_held.r_left(),
                            LDown => button_current_held.l_down(),
                            RDown => button_current_held.r_down(),
                            LRight => button_current_held.l_right(),
                            RRight => button_current_held.r_right(),
                            LUp => button_current_held.l_up(),
                            RUp => button_current_held.r_up(),
                        };
                        if still_held {
                            *frames += 1;
                        } else {
                            *frames = 0;
                        }
                    });

                let mut app = lock_write(&QUICK_MENU_APP);
                button_config::button_mapping(ButtonConfig::A, style, button_presses).then(|| {
                    app.on_a();
                    received_input = true;
                });
                button_config::button_mapping(ButtonConfig::B, style, button_presses).then(|| {
                    received_input = true;
                    app.on_b();
                    if app.page == AppPage::CLOSE {
                        // Leave menu.
                        frame_counter::start_counting(*MENU_CLOSE_FRAME_COUNTER);
                        assign(&QUICK_MENU_ACTIVE, false);
                        let menu_json = app.get_serialized_settings_with_defaults();
                        set_menu_from_json(&menu_json);

                        let mut event_queue_lock = lock_write(&EVENT_QUEUE);
                        (*event_queue_lock).push(Event::menu_open(menu_json));
                        drop(event_queue_lock);
                    }
                });
                button_config::button_mapping(ButtonConfig::X, style, button_presses).then(|| {
                    app.on_x();
                    received_input = true;
                });
                button_config::button_mapping(ButtonConfig::Y, style, button_presses).then(|| {
                    app.on_y();
                    received_input = true;
                });

                button_config::button_mapping(ButtonConfig::ZL, style, button_presses).then(|| {
                    app.on_zl();
                    received_input = true;
                });
                button_config::button_mapping(ButtonConfig::ZR, style, button_presses).then(|| {
                    app.on_zr();
                    received_input = true;
                });
                button_config::button_mapping(ButtonConfig::R, style, button_presses).then(|| {
                    app.on_r();
                    received_input = true;
                });

                let hold_condition = |direction_button| {
                    direction_hold_frames[direction_button] > DIRECTION_HOLD_REPEAT_FRAMES
                };
                (button_presses.dpad_left()
                    || button_presses.l_left()
                    || button_presses.r_left()
                    || [LLeft, RLeft].iter().any(hold_condition))
                .then(|| {
                    received_input = true;
                    app.on_left();
                });
                (button_presses.dpad_right()
                    || button_presses.l_right()
                    || button_presses.r_right()
                    || [LRight, RRight].iter().any(hold_condition))
                .then(|| {
                    received_input = true;
                    app.on_right();
                });
                (button_presses.dpad_up()
                    || button_presses.l_up()
                    || button_presses.r_up()
                    || [LUp, RUp].iter().any(hold_condition))
                .then(|| {
                    received_input = true;
                    app.on_up();
                });
                (button_presses.dpad_down()
                    || button_presses.l_down()
                    || button_presses.r_down()
                    || [LDown, RDown].iter().any(hold_condition))
                .then(|| {
                    received_input = true;
                    app.on_down();
                });

                if received_input {
                    direction_hold_frames.iter_mut().for_each(|(_, f)| *f = 0);
                    assign(&MENU_RECEIVED_INPUT, true);
                }
            }
        }
    }
}
