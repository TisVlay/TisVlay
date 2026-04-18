// SPDX-License-Identifier: GPL-2.0

/*
 * C helpers for the tisvlay virtual backlight kernel module.
 *
 * Provides:
 * 1. Backlight device registration/unregistration (the kernel backlight
 *    subsystem has no Rust abstractions yet).
 * 2. An event queue for forward-path brightness changes (KDE writes to
 *    sysfs → daemon reads from /dev/tisvlay-bl). Uses a wait queue for
 *    blocking reads since that primitive also lacks Rust bindings.
 * 3. A binary `edid` sysfs attribute on each backlight device so
 *    PowerDevil can identify which DRM output it corresponds to.
 */

#include <linux/backlight.h>
#include <linux/device.h>
#include <linux/list.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/string.h>
#include <linux/sysfs.h>
#include <linux/wait.h>

/* Maximum TV ID length (must match the Rust side). */
#define TISVLAY_ID_MAX 64

/* Maximum EDID size (base block + one extension). */
#define TISVLAY_EDID_MAX 256

/* Maximum number of backlight devices. */
#define TISVLAY_MAX_DEVICES 16

/* ── Event queue (forward path: KDE → daemon) ───────────────────── */

struct tisvlay_event {
	struct list_head list;
	char tv_id[TISVLAY_ID_MAX];
	int brightness;
};

static DEFINE_SPINLOCK(event_lock);
static LIST_HEAD(event_list);
static DECLARE_WAIT_QUEUE_HEAD(event_waitq);
/* Set to true when the module is unloading so blocked readers wake. */
static bool shutting_down;

/* Push a brightness-changed event. Called from backlight update_status. */
static void push_event(const char *tv_id, int brightness)
{
	struct tisvlay_event *evt;

	evt = kzalloc(sizeof(*evt), GFP_ATOMIC);
	if (!evt)
		return;

	strscpy(evt->tv_id, tv_id, TISVLAY_ID_MAX);
	evt->brightness = brightness;

	spin_lock(&event_lock);
	list_add_tail(&evt->list, &event_list);
	spin_unlock(&event_lock);

	wake_up_interruptible(&event_waitq);
}

/*
 * Pop one event. Returns 0 on success, -EAGAIN if empty, or
 * -ERESTARTSYS if interrupted. If `block` is true, waits for
 * an event or shutdown.
 */
int tisvlay_pop_event(char *tv_id, int *brightness, bool block)
{
	struct tisvlay_event *evt;
	int ret;

	if (block) {
		ret = wait_event_interruptible(event_waitq,
			shutting_down || !list_empty(&event_list));
		if (ret)
			return ret;
		if (shutting_down)
			return -ENODEV;
	}

	spin_lock(&event_lock);
	if (list_empty(&event_list)) {
		spin_unlock(&event_lock);
		return -EAGAIN;
	}
	evt = list_first_entry(&event_list, struct tisvlay_event, list);
	list_del(&evt->list);
	spin_unlock(&event_lock);

	strscpy(tv_id, evt->tv_id, TISVLAY_ID_MAX);
	*brightness = evt->brightness;
	kfree(evt);
	return 0;
}

void tisvlay_events_shutdown(void)
{
	struct tisvlay_event *evt, *tmp;

	shutting_down = true;
	wake_up_interruptible(&event_waitq);

	spin_lock(&event_lock);
	list_for_each_entry_safe(evt, tmp, &event_list, list) {
		list_del(&evt->list);
		kfree(evt);
	}
	spin_unlock(&event_lock);
}

void tisvlay_events_init(void)
{
	shutting_down = false;
}

/* ── Backlight devices ──────────────────────────────────────────── */

struct tisvlay_bl_device {
	struct backlight_device *bd;
	char tv_id[TISVLAY_ID_MAX];
	u8 edid[TISVLAY_EDID_MAX];
	int edid_len;
	bool in_use;
};

static struct tisvlay_bl_device devices[TISVLAY_MAX_DEVICES];
static DEFINE_MUTEX(devices_lock);

/* ── EDID sysfs attribute ──────────────────────────────────────── */

static ssize_t edid_show(struct device *dev, struct device_attribute *attr,
			 char *buf)
{
	struct backlight_device *bd = to_backlight_device(dev);
	struct tisvlay_bl_device *tdev = bl_get_data(bd);

	if (!tdev || tdev->edid_len <= 0)
		return 0;

	memcpy(buf, tdev->edid, tdev->edid_len);
	return tdev->edid_len;
}

static DEVICE_ATTR_RO(edid);

/* ── Backlight ops ─────────────────────────────────────────────── */

static int tisvlay_bl_update_status(struct backlight_device *bd)
{
	struct tisvlay_bl_device *dev = bl_get_data(bd);

	if (dev)
		push_event(dev->tv_id, bd->props.brightness);
	return 0;
}

static int tisvlay_bl_get_brightness(struct backlight_device *bd)
{
	return bd->props.brightness;
}

static const struct backlight_ops tisvlay_bl_ops = {
	.update_status = tisvlay_bl_update_status,
	.get_brightness = tisvlay_bl_get_brightness,
};

int tisvlay_bl_create(const char *tv_id, int max_brightness,
		      int initial_brightness,
		      const u8 *edid_data, int edid_len)
{
	struct backlight_properties props;
	struct backlight_device *bd;
	struct tisvlay_bl_device *dev = NULL;
	char name[TISVLAY_ID_MAX + 8];
	int i, ret = -ENOMEM;

	if (!tv_id || !tv_id[0] || max_brightness <= 0)
		return -EINVAL;
	if (edid_len < 0 || edid_len > TISVLAY_EDID_MAX)
		return -EINVAL;

	mutex_lock(&devices_lock);

	/* Check for duplicates. */
	for (i = 0; i < TISVLAY_MAX_DEVICES; i++) {
		if (devices[i].in_use &&
		    strncmp(devices[i].tv_id, tv_id, TISVLAY_ID_MAX) == 0) {
			ret = -EEXIST;
			goto out;
		}
	}

	/* Find a free slot. */
	for (i = 0; i < TISVLAY_MAX_DEVICES; i++) {
		if (!devices[i].in_use) {
			dev = &devices[i];
			break;
		}
	}
	if (!dev) {
		ret = -ENOSPC;
		goto out;
	}

	snprintf(name, sizeof(name), "tisvlay-%s", tv_id);

	memset(&props, 0, sizeof(props));
	props.type = BACKLIGHT_PLATFORM;
	props.max_brightness = max_brightness;
	props.brightness = initial_brightness;

	strscpy(dev->tv_id, tv_id, TISVLAY_ID_MAX);

	/* Store EDID before registration so the sysfs attr can read it. */
	if (edid_data && edid_len > 0) {
		memcpy(dev->edid, edid_data, edid_len);
		dev->edid_len = edid_len;
	} else {
		dev->edid_len = 0;
	}

	bd = backlight_device_register(name, NULL, dev,
				       &tisvlay_bl_ops, &props);
	if (IS_ERR(bd)) {
		ret = PTR_ERR(bd);
		memset(dev, 0, sizeof(*dev));
		goto out;
	}

	/* Expose the EDID as a sysfs attribute on the backlight device. */
	if (dev->edid_len > 0) {
		ret = device_create_file(&bd->dev, &dev_attr_edid);
		if (ret) {
			pr_warn("tisvlay: failed to create edid sysfs attr: %d\n", ret);
			/* Non-fatal — brightness still works, just no EDID matching. */
			ret = 0;
		}
	}

	dev->bd = bd;
	dev->in_use = true;
	ret = 0;

out:
	mutex_unlock(&devices_lock);
	return ret;
}

int tisvlay_bl_destroy(const char *tv_id)
{
	int i, ret = -ENOENT;

	if (!tv_id)
		return -EINVAL;

	mutex_lock(&devices_lock);
	for (i = 0; i < TISVLAY_MAX_DEVICES; i++) {
		if (devices[i].in_use &&
		    strncmp(devices[i].tv_id, tv_id, TISVLAY_ID_MAX) == 0) {
			if (devices[i].edid_len > 0)
				device_remove_file(&devices[i].bd->dev, &dev_attr_edid);
			backlight_device_unregister(devices[i].bd);
			memset(&devices[i], 0, sizeof(devices[i]));
			ret = 0;
			break;
		}
	}
	mutex_unlock(&devices_lock);
	return ret;
}

int tisvlay_bl_set(const char *tv_id, int brightness)
{
	int i, ret = -ENOENT;

	if (!tv_id)
		return -EINVAL;

	mutex_lock(&devices_lock);
	for (i = 0; i < TISVLAY_MAX_DEVICES; i++) {
		if (devices[i].in_use &&
		    strncmp(devices[i].tv_id, tv_id, TISVLAY_ID_MAX) == 0) {
			devices[i].bd->props.brightness = brightness;
			/*
			 * Use backlight_force_update, not backlight_update_status:
			 * the latter invokes ops->update_status (our push_event),
			 * which echoes this daemon-initiated write back as a
			 * forward event and causes an oscillation loop with the
			 * subscription-driven reverse path. force_update emits the
			 * sysfs/uevent notification PowerDevil watches without
			 * firing the ops callback.
			 */
			backlight_force_update(devices[i].bd,
					       BACKLIGHT_UPDATE_HOTKEY);
			ret = 0;
			break;
		}
	}
	mutex_unlock(&devices_lock);
	return ret;
}

void tisvlay_bl_destroy_all(void)
{
	int i;

	mutex_lock(&devices_lock);
	for (i = 0; i < TISVLAY_MAX_DEVICES; i++) {
		if (devices[i].in_use) {
			if (devices[i].edid_len > 0)
				device_remove_file(&devices[i].bd->dev, &dev_attr_edid);
			backlight_device_unregister(devices[i].bd);
			memset(&devices[i], 0, sizeof(devices[i]));
		}
	}
	mutex_unlock(&devices_lock);
}
