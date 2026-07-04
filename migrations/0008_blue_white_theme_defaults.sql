UPDATE theme_settings
SET value_json = jsonb_build_object(
    'mode', COALESCE(value_json ->> 'mode', 'light'),
    'preset', COALESCE(value_json ->> 'preset', 'blue_white'),
    'radius', COALESCE(value_json -> 'radius', '16'::jsonb),
    'blur', COALESCE(value_json -> 'blur', '18'::jsonb),
    'mobile_blur', COALESCE(value_json -> 'mobile_blur', '10'::jsonb),
    'card_opacity', COALESCE(value_json -> 'card_opacity', '0.72'::jsonb),
    'primary_color', COALESCE(value_json ->> 'primary_color', '#1d6fd8'),
    'accent_color', COALESCE(value_json ->> 'accent_color', '#58b7ff'),
    'background_color', COALESCE(value_json ->> 'background_color', '#eef7ff'),
    'surface_color', COALESCE(value_json ->> 'surface_color', '#ffffff'),
    'font', COALESCE(value_json ->> 'font', '系统圆体'),
    'background_image', COALESCE(value_json ->> 'background_image', ''),
    'simplify_mobile_effects', COALESCE(value_json -> 'simplify_mobile_effects', 'true'::jsonb)
)
WHERE key = 'theme';
