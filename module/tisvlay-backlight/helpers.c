// SPDX-License-Identifier: GPL-2.0

/*
 * C helpers for backlight subsystem access from Rust.
 *
 * The kernel's backlight API has no Rust abstractions yet, so we
 * provide thin wrappers callable from the Rust side of the module.
 */

#include <linux/backlight.h>
#include <linux/device.h>
#include <linux/export.h>
#include <linux/slab.h>

struct tisvlay_bl_device;

/* Callback signature the Rust side implements. */
typedef int (*tisvlay_bl_update_fn)(struct tisvlay_bl_device *dev);
typedef int (*tisvlay_bl_get_fn)(struct tisvlay_bl_device *dev);

struct tisvlay_bl_device {
	struct backlight_device *bd;
	tisvlay_bl_update_fn update_fn;
	tisvlay_bl_get_fn get_fn;
	void *rust_data;
};

static int tisvlay_bl_update_status(struct backlight_device *bd)
{
	struct tisvlay_bl_device *dev = bl_get_data(bd);
	if (dev && dev->update_fn)
		return dev->update_fn(dev);
	return 0;
}

static int tisvlay_bl_get_brightness(struct backlight_device *bd)
{
	struct tisvlay_bl_device *dev = bl_get_data(bd);
	if (dev && dev->get_fn)
		return dev->get_fn(dev);
	return bd->props.brightness;
}

static const struct backlight_ops tisvlay_bl_ops = {
	.update_status = tisvlay_bl_update_status,
	.get_brightness = tisvlay_bl_get_brightness,
};

struct tisvlay_bl_device *tisvlay_bl_register(
	struct device *parent,
	const char *name,
	int max_brightness,
	int initial_brightness,
	void *rust_data,
	tisvlay_bl_update_fn update_fn,
	tisvlay_bl_get_fn get_fn)
{
	struct backlight_properties props;
	struct backlight_device *bd;
	struct tisvlay_bl_device *dev;

	dev = kzalloc(sizeof(*dev), GFP_KERNEL);
	if (!dev)
		return ERR_PTR(-ENOMEM);

	dev->update_fn = update_fn;
	dev->get_fn = get_fn;
	dev->rust_data = rust_data;

	memset(&props, 0, sizeof(props));
	props.type = BACKLIGHT_PLATFORM;
	props.max_brightness = max_brightness;
	props.brightness = initial_brightness;

	bd = backlight_device_register(name, parent, dev, &tisvlay_bl_ops, &props);
	if (IS_ERR(bd)) {
		kfree(dev);
		return ERR_CAST(bd);
	}

	dev->bd = bd;
	return dev;
}

void tisvlay_bl_unregister(struct tisvlay_bl_device *dev)
{
	if (!dev)
		return;
	backlight_device_unregister(dev->bd);
	kfree(dev);
}

void tisvlay_bl_set_brightness(struct tisvlay_bl_device *dev, int brightness)
{
	if (!dev || !dev->bd)
		return;
	dev->bd->props.brightness = brightness;
	backlight_update_status(dev->bd);
}

int tisvlay_bl_get_current(struct tisvlay_bl_device *dev)
{
	if (!dev || !dev->bd)
		return 0;
	return dev->bd->props.brightness;
}

void *tisvlay_bl_get_rust_data(struct tisvlay_bl_device *dev)
{
	return dev ? dev->rust_data : NULL;
}
