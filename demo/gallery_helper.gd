extends RefCounted
## Save a captured JPG / recorded mp4 into the phone's shared gallery (MediaStore) from pure
## GDScript — the Godot port of VRCameraUnity's GalleryHelper.kt, with no Java/Kotlin plugin:
## Godot 4.4+'s JavaClassWrapper drives the Android MediaStore API directly (constructors are
## called as a method named after the class, e.g. ContentValues.ContentValues()). Demo-only glue:
## the addon's capture/recorder components just return the saved file's path; what to do with it
## is the app's choice.
##
## min_sdk is 29, so only the scoped-storage flow exists: MediaStore insert with RELATIVE_PATH +
## IS_PENDING, write through the resolver's OutputStream, then clear IS_PENDING. The app's own
## MediaStore inserts need no runtime permission on API 29+.

## Copy an image at `src_path` into the phone gallery under Pictures/godot-xreal.
## Returns whether it was saved. No-op off Android.
static func save_image(src_path: String, mime := "image/jpeg") -> bool:
	return _save(src_path, mime, false)

## Copy a video at `src_path` into the phone gallery under Movies/godot-xreal.
## Returns whether it was saved. No-op off Android.
static func save_video(src_path: String, mime := "video/mp4") -> bool:
	return _save(src_path, mime, true)

## Copy chunk size: recordings can run to hundreds of MB, so never load the whole file at once.
const _CHUNK := 4 * 1024 * 1024

static func _save(src_path: String, mime: String, is_video: bool) -> bool:
	if OS.get_name() != "Android" or not Engine.has_singleton(&"AndroidRuntime"):
		return false
	var src := FileAccess.open(src_path, FileAccess.READ)
	if src == null or src.get_length() == 0:
		push_warning("[demo-gallery] cannot read %s" % src_path)
		return false
	var activity = Engine.get_singleton(&"AndroidRuntime").getActivity()
	var content_values_class := JavaClassWrapper.wrap("android.content.ContentValues")
	var media_class := JavaClassWrapper.wrap(
		"android.provider.MediaStore$Video$Media" if is_video
		else "android.provider.MediaStore$Images$Media")
	if activity == null or content_values_class == null or media_class == null:
		push_warning("[demo-gallery] Android runtime/classes unavailable")
		return false
	var rel_dir := "Movies/godot-xreal" if is_video else "Pictures/godot-xreal"
	var resolver = activity.getContentResolver()
	# Column-name string literals are the stable public values of MediaStore.MediaColumns.*
	# (JavaClassWrapper reaches methods, not static fields).
	var values = content_values_class.ContentValues()
	values.put("_display_name", src_path.get_file())
	values.put("mime_type", mime)
	values.put("relative_path", rel_dir)
	values.put("is_pending", 1)
	# MediaStore.VOLUME_EXTERNAL_PRIMARY == "external_primary"
	var item = resolver.insert(media_class.getContentUri("external_primary"), values)
	if item == null:
		push_warning("[demo-gallery] MediaStore insert failed for %s" % src_path.get_file())
		return false
	# NB: JavaClassWrapper cannot pass null for String / String[] parameters ("Cannot convert
	# argument from Nil to String"), so the no-selection update/delete calls below use "" plus an
	# empty PackedStringArray instead — providers treat an empty selection like a null one.
	var no_where := ""
	var no_args := PackedStringArray()
	var out = resolver.openOutputStream(item)
	if out == null:
		resolver.delete(item, no_where, no_args)
		push_warning("[demo-gallery] openOutputStream failed for %s" % src_path.get_file())
		return false
	while src.get_position() < src.get_length():
		out.write(src.get_buffer(_CHUNK))  # PackedByteArray -> byte[]
	out.flush()
	out.close()
	values.clear()
	values.put("is_pending", 0)
	# Clear IS_PENDING to publish the item — while it is pending, other apps (the gallery) can't
	# see it (it sits on disk as ".pending-<epoch>-<name>"). Verify the row really updated.
	var updated = resolver.update(item, values, no_where, no_args)
	if updated == null or int(updated) < 1:
		push_warning("[demo-gallery] IS_PENDING clear failed for %s — still hidden in the gallery" % src_path.get_file())
		return false
	print("[demo-gallery] saved -> %s/%s" % [rel_dir, src_path.get_file()])
	return true
