// The code here is based on the color_namer code in trials folder

// It uses css colors mapped to simple names fit to 3d printing context 
// of user picking a color and deduction of color name from his selection
// So colors of gold/silver/etc. which can't rely on user selection to deduce
// accurate color name, because he could choose yellow and accidentally land on 
// some rgb value which matches gold. 

use num_traits::Float;
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy)]
struct RGB {
    r: u8,
    g: u8,
    b: u8,
}

impl RGB {
    const fn new(r: u8, g: u8, b: u8) -> Self {
        RGB { r, g, b }
    }

    // Convert RGB to Lab color space for better perceptual color distance calculation
    #[allow(clippy::wrong_self_convention)]
    fn to_lab(&self) -> (f32, f32, f32) {
        // First convert RGB to XYZ
        let r = srgb_to_linear(self.r as f32 / 255.0);
        let g = srgb_to_linear(self.g as f32 / 255.0);
        let b = srgb_to_linear(self.b as f32 / 255.0);

        // Standard RGB to XYZ matrix conversion
        let x = 0.4124 * r + 0.3576 * g + 0.1805 * b;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let z = 0.0193 * r + 0.1192 * g + 0.9505 * b;

        // XYZ to Lab
        let x = x / 0.95047;
        let y = y / 1.0;
        let z = z / 1.08883;

        let x = if x > 0.008856 {
            x.powf(1.0/3.0)
        } else {
            (7.787 * x) + (16.0 / 116.0)
        };
        let y = if y > 0.008856 {
            y.powf(1.0 / 3.0)
        } else {
            (7.787 * y) + (16.0 / 116.0)
        };
        let z = if z > 0.008856 {
            z.powf(1.0 / 3.0)
        } else {
            (7.787 * z) + (16.0 / 116.0)
        };

        let l = (116.0 * y) - 16.0;
        let a = 500.0 * (x - y);
        let b = 200.0 * (y - z);

        (l, a, b)
    }

    // Calculate perceptual distance between two colors using CIE76 Delta-E formula
    fn perceptual_distance(&self, other: &RGB) -> f32 {
        let (l1, a1, b1) = self.to_lab();
        let (l2, a2, b2) = other.to_lab();

        let dl = l1 - l2;
        let da = a1 - a2;
        let db = b1 - b2;

        (dl * dl + da * da + db * db).sqrt()
    }
}

// Helper function to convert sRGB component to linear RGB
fn srgb_to_linear(component: f32) -> f32 {
    if component <= 0.04045 {
        component / 12.92
    } else {
        ((component + 0.055) / 1.055).powf(2.4)
    }
}

// Main function to get the name of a color from RGB values
pub fn get_color_name(r: u8, g: u8, b: u8) -> (&'static str, &'static str) {
    let input_color = RGB::new(r, g, b);

    let color_table = COLOR_NAMES;

    let mut closest_simple_color = "unknown";
    let mut closest_complex_color = "unknown";
    let mut min_distance = f32::MAX;

    for (complexname, color, simplename) in &color_table {
        let distance = input_color.perceptual_distance(color);
        if distance < min_distance {
            min_distance = distance;
            closest_simple_color = simplename;
            closest_complex_color = complexname;
        }
    }

    (closest_simple_color, closest_complex_color)
}

static COLOR_NAMES: [(&str, RGB, &str); 147] = [
    // css name, rgb, simple color name
    ("aqua", RGB::new(0, 255, 255), "Cyan"),
    ("aliceblue", RGB::new(240, 248, 255), "Light Blue"),
    ("antiquewhite", RGB::new(250, 235, 215), "Beige"),
    ("black", RGB::new(0, 0, 0), "Black"),
    ("blue", RGB::new(0, 0, 255), "Blue"),
    ("cyan", RGB::new(0, 255, 255), "Cyan"),
    ("darkblue", RGB::new(0, 0, 139), "Dark Blue"),
    ("darkcyan", RGB::new(0, 139, 139), "Teal"),
    ("darkgreen", RGB::new(0, 100, 0), "Dark Green"),
    ("darkturquoise", RGB::new(0, 206, 209), "Cyan"),
    ("deepskyblue", RGB::new(0, 191, 255), "Light Blue"),
    ("green", RGB::new(0, 128, 0), "Green"),
    ("lime", RGB::new(0, 255, 0), "Light Green"),
    ("mediumblue", RGB::new(0, 0, 205), "Blue"),
    ("mediumspringgreen", RGB::new(0, 250, 154), "Light Green"),
    ("navy", RGB::new(0, 0, 128), "Dark Blue"),
    ("springgreen", RGB::new(0, 255, 127), "Light Green"),
    ("teal", RGB::new(0, 128, 128), "Teal"),
    ("midnightblue", RGB::new(25, 25, 112), "Dark Blue"),
    ("dodgerblue", RGB::new(30, 144, 255), "Blue"),
    ("lightseagreen", RGB::new(32, 178, 170), "Teal"),
    ("forestgreen", RGB::new(34, 139, 34), "Green"),
    ("seagreen", RGB::new(46, 139, 87), "Green"),
    ("darkslategray", RGB::new(47, 79, 79), "Gray"),
    ("darkslategrey", RGB::new(47, 79, 79), "Gray"),
    ("limegreen", RGB::new(50, 205, 50), "Light Green"),
    ("mediumseagreen", RGB::new(60, 179, 113), "Green"),
    ("turquoise", RGB::new(64, 224, 208), "Cyan"),
    ("royalblue", RGB::new(65, 105, 225), "Blue"),
    ("steelblue", RGB::new(70, 130, 180), "Blue"),
    ("darkslateblue", RGB::new(72, 61, 139), "Purple"),
    ("mediumturquoise", RGB::new(72, 209, 204), "Cyan"),
    ("indigo", RGB::new(75, 0, 130), "Purple"),
    ("darkolivegreen", RGB::new(85, 107, 47), "Olive"),
    ("cadetblue", RGB::new(95, 158, 160), "Teal"),
    ("cornflowerblue", RGB::new(100, 149, 237), "Blue"),
    ("mediumaquamarine", RGB::new(102, 205, 170), "Light Green"),
    ("dimgray", RGB::new(105, 105, 105), "Gray"),
    ("dimgrey", RGB::new(105, 105, 105), "Gray"),
    ("slateblue", RGB::new(106, 90, 205), "Purple"),
    ("olivedrab", RGB::new(107, 142, 35), "Olive"),
    ("slategray", RGB::new(112, 128, 144), "Gray"),
    ("slategrey", RGB::new(112, 128, 144), "Gray"),
    ("lightslategray", RGB::new(119, 136, 153), "Gray"),
    ("lightslategrey", RGB::new(119, 136, 153), "Gray"),
    ("mediumslateblue", RGB::new(123, 104, 238), "Purple"),
    ("lawngreen", RGB::new(124, 252, 0), "Light Green"),
    ("aquamarine", RGB::new(127, 255, 212), "Cyan"),
    ("chartreuse", RGB::new(127, 255, 0), "Light Green"),
    ("gray", RGB::new(128, 128, 128), "Gray"),
    ("grey", RGB::new(128, 128, 128), "Gray"),
    ("maroon", RGB::new(128, 0, 0), "Dark Red"),
    ("olive", RGB::new(128, 128, 0), "Olive"),
    ("purple", RGB::new(128, 0, 128), "Purple"),
    ("lightskyblue", RGB::new(135, 206, 250), "Light Blue"),
    ("skyblue", RGB::new(135, 206, 235), "Light Blue"),
    ("blueviolet", RGB::new(138, 43, 226), "Purple"),
    ("darkmagenta", RGB::new(139, 0, 139), "Purple"),
    ("darkred", RGB::new(139, 0, 0), "Dark Red"),
    ("saddlebrown", RGB::new(139, 69, 19), "Brown"),
    ("darkseagreen", RGB::new(143, 188, 143), "Light Green"),
    ("lightgreen", RGB::new(144, 238, 144), "Light Green"),
    ("mediumpurple", RGB::new(147, 112, 219), "Purple"),
    ("darkviolet", RGB::new(148, 0, 211), "Violet"),
    ("palegreen", RGB::new(152, 251, 152), "Light Green"),
    ("darkorchid", RGB::new(153, 50, 204), "Purple"),
    ("yellowgreen", RGB::new(154, 205, 50), "Light Green"),
    ("sienna", RGB::new(160, 82, 45), "Brown"),
    ("brown", RGB::new(165, 42, 42), "Brown"),
    ("darkgray", RGB::new(169, 169, 169), "Gray"),
    ("darkgrey", RGB::new(169, 169, 169), "Gray"),
    ("greenyellow", RGB::new(173, 255, 47), "Yellow"),
    ("lightblue", RGB::new(173, 216, 230), "Light Blue"),
    ("paleturquoise", RGB::new(175, 238, 238), "Light Blue"),
    ("lightsteelblue", RGB::new(176, 196, 222), "Light Blue"),
    ("powderblue", RGB::new(176, 224, 230), "Light Blue"),
    ("firebrick", RGB::new(178, 34, 34), "Dark Red"),
    ("darkgoldenrod", RGB::new(184, 134, 11), "Brown"),
    ("mediumorchid", RGB::new(186, 85, 211), "Purple"),
    ("rosybrown", RGB::new(188, 143, 143), "Light Red"),
    ("darkkhaki", RGB::new(189, 183, 107), "Olive"),
    ("silver", RGB::new(192, 192, 192), "Gray"),
    ("mediumvioletred", RGB::new(199, 21, 133), "Magenta"),
    ("indianred", RGB::new(205, 92, 92), "Light Red"),
    ("peru", RGB::new(205, 133, 63), "Brown"),
    ("chocolate", RGB::new(210, 105, 30), "Brown"),
    ("tan", RGB::new(210, 180, 140), "Beige"),
    ("lightgray", RGB::new(211, 211, 211), "Gray"),
    ("lightgrey", RGB::new(211, 211, 211), "Gray"),
    ("thistle", RGB::new(216, 191, 216), "Violet"),
    ("goldenrod", RGB::new(218, 165, 32), "Yellow"),
    ("orchid", RGB::new(218, 112, 214), "Pink"),
    ("palevioletred", RGB::new(219, 112, 147), "Pink"),
    ("crimson", RGB::new(220, 20, 60), "Red"),
    ("gainsboro", RGB::new(220, 220, 220), "Gray"),
    ("plum", RGB::new(221, 160, 221), "Pink"),
    ("burlywood", RGB::new(222, 184, 135), "Beige"),
    ("lightcyan", RGB::new(224, 255, 255), "Cyan"),
    ("lavender", RGB::new(230, 230, 250), "Light Blue"),
    ("darksalmon", RGB::new(233, 150, 122), "Light Red"),
    ("palegoldenrod", RGB::new(238, 232, 170), "Yellow"),
    ("violet", RGB::new(238, 130, 238), "Violet"),
    ("azure", RGB::new(240, 255, 255), "Light Blue"),
    ("honeydew", RGB::new(240, 255, 240), "Light Green"),
    ("khaki", RGB::new(240, 230, 140), "Yellow"),
    ("lightcoral", RGB::new(240, 128, 128), "Light Red"),
    ("sandybrown", RGB::new(244, 164, 96), "Orange"),
    ("beige", RGB::new(245, 245, 220), "Beige"),
    ("mintcream", RGB::new(245, 255, 250), "White"),
    ("wheat", RGB::new(245, 222, 179), "Beige"),
    ("whitesmoke", RGB::new(245, 245, 245), "White"),
    ("ghostwhite", RGB::new(248, 248, 255), "White"),
    ("lightgoldenrodyellow", RGB::new(250, 250, 210), "Yellow"),
    ("linen", RGB::new(250, 240, 230), "White"),
    ("salmon", RGB::new(250, 128, 114), "Light Red"),
    ("oldlace", RGB::new(253, 245, 230), "White"),
    ("bisque", RGB::new(255, 228, 196), "Beige"),
    ("blanchedalmond", RGB::new(255, 235, 205), "Beige"),
    ("coral", RGB::new(255, 127, 80), "Orange"),
    ("cornsilk", RGB::new(255, 248, 220), "Yellow"),
    ("darkorange", RGB::new(255, 140, 0), "Orange"),
    ("deeppink", RGB::new(255, 20, 147), "Pink"),
    ("floralwhite", RGB::new(255, 250, 240), "White"),
    ("fuchsia", RGB::new(255, 0, 255), "Magenta"),
    ("gold", RGB::new(255, 215, 0), "Yellow"),
    ("hotpink", RGB::new(255, 105, 180), "Pink"),
    ("ivory", RGB::new(255, 255, 240), "White"),
    ("lavenderblush", RGB::new(255, 240, 245), "White"),
    ("lemonchiffon", RGB::new(255, 250, 205), "Yellow"),
    ("lightpink", RGB::new(255, 182, 193), "Pink"),
    ("lightsalmon", RGB::new(255, 160, 122), "Orange"),
    ("lightyellow", RGB::new(255, 255, 224), "Yellow"),
    ("magenta", RGB::new(255, 0, 255), "Magenta"),
    ("mistyrose", RGB::new(255, 228, 225), "Pink"),
    ("moccasin", RGB::new(255, 228, 181), "Beige"),
    ("navajowhite", RGB::new(255, 222, 173), "Beige"),
    ("orange", RGB::new(255, 165, 0), "Orange"),
    ("orangered", RGB::new(255, 69, 0), "Orange"),
    ("papayawhip", RGB::new(255, 239, 213), "Beige"),
    ("peachpuff", RGB::new(255, 218, 185), "Beige"),
    ("pink", RGB::new(255, 192, 203), "Pink"),
    ("red", RGB::new(255, 0, 0), "Red"),
    ("seashell", RGB::new(255, 245, 238), "White"),
    ("snow", RGB::new(255, 250, 250), "White"),
    ("tomato", RGB::new(255, 99, 71), "Red"),
    ("white", RGB::new(255, 255, 255), "White"),
    ("yellow", RGB::new(255, 255, 0), "Yellow"),
];
