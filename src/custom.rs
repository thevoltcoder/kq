use super::*;
use std::cell::RefCell;

fn format_fader(v: f64) -> String {
    if v <= -79.9 {
        "-inf".to_string()
    } else if v > 0.0 {
        format!("+{:.1}", v)
    } else if v == 0.0 {
        "0.0".to_string()
    } else {
        format!("{:.1}", v)
    }
}

fn parse_fader(s: &str) -> Option<f64> {
    let t = s.trim().replace('\u{2212}', "-").to_lowercase();
    if t == "-inf" || t == "inf" || t == "-∞" || t == "∞" {
        return Some(f64::NEG_INFINITY);
    }
    let (positive, rest) = match t.strip_prefix('+') {
        Some(r) => (true, r.trim()),
        None => (false, t.as_str()),
    };
    let mag = match rest.parse::<i32>() {
        Ok(i) => (i as f64).abs(),
        Err(_) => rest.parse::<f64>().ok()?.abs(),
    };
    Some(if positive { mag } else { -mag })
}

pub struct FaderSliderIvars {
    pub slider: OnceCell<Retained<NSSlider>>,
    pub text_field: OnceCell<Retained<NSTextField>>,
    pub on_change: RefCell<Option<Box<dyn Fn(f64)>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = FaderSliderIvars]
    pub struct FaderSlider;

    unsafe impl NSObjectProtocol for FaderSlider {}

    impl FaderSlider {
        #[unsafe(method(sliderChanged:))]
        fn slider_changed(&self, sender: &NSSlider) {
            let val = sender.doubleValue();
            if let Some(tf) = self.ivars().text_field.get() {
                tf.setStringValue(&NSString::from_str(&format_fader(val)));
            }
            self.fire(val);
        }

        #[unsafe(method(textChanged:))]
        fn text_changed(&self, sender: &NSTextField) {
            let Some(slider) = self.ivars().slider.get() else { return };

            let current = slider.doubleValue();
            let parsed = parse_fader(&sender.stringValue().to_string());

            let val = parsed.unwrap_or(current);
            let clamped = val.clamp(slider.minValue(), slider.maxValue());

            slider.setDoubleValue(clamped);
            sender.setStringValue(&NSString::from_str(&format_fader(clamped)));
            self.fire(clamped);
        }
    }
);

impl FaderSlider {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(FaderSliderIvars {
            slider: OnceCell::new(),
            text_field: OnceCell::new(),
            on_change: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    pub fn set_on_change(&self, f: impl Fn(f64) + 'static) {
        *self.ivars().on_change.borrow_mut() = Some(Box::new(f));
    }

    fn fire(&self, v: f64) {
        if let Some(cb) = self.ivars().on_change.borrow().as_ref() {
            cb(v);
        }
    }

    pub fn attach(&self, slider: Retained<NSSlider>, text_field: Retained<NSTextField>) {
        unsafe {
            slider.setTarget(Some(self));
            slider.setAction(Some(sel!(sliderChanged:)));
            slider.setContinuous(true);

            text_field.setTarget(Some(self));
            text_field.setAction(Some(sel!(textChanged:)));
            text_field.setStringValue(&NSString::from_str(&format_fader(slider.doubleValue())));
        }
        let _ = self.ivars().slider.set(slider);
        let _ = self.ivars().text_field.set(text_field);
    }
}
