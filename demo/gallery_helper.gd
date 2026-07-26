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
##
## This is a move, not a copy: once the item is published the app-private original is deleted. It
## used to be left behind, which meant every capture and recording existed twice — invisibly, since
## user:// is not browsable — and only the gallery copy was ever reachable. On this device that had
## quietly reached 47 MB of duplicates.

## Move an image at `src_path` into the phone gallery under Pictures/godot-xreal.
## Returns whether it was saved. No-op off Android.
static func save_image(src_path: String, mime := "image/jpeg") -> bool:
	return _save(src_path, mime, false)

## Move a video at `src_path` into the phone gallery under Movies/godot-xreal.
## Returns whether it was saved. No-op off Android.
static func save_video(src_path: String, mime := "video/mp4") -> bool:
	return _save(src_path, mime, true)

## Copy chunk size: recordings can run to hundreds of MB, so never load the whole file at once.
const _CHUNK := 4 * 1024 * 1024

## Why the Java exception raised by the call just made, as a warning suffix — or "" if there was
## none. Always call it IMMEDIATELY after the call being checked, and before branching on that
## call's result (`a == null or _java_reason()` would short-circuit past the reason).
##
## JavaClassWrapper swallows every exception: the engine does ExceptionClear() and the call just
## returns null, so a throwing MediaStore write is otherwise indistinguishable from a working one.
## That is not cosmetic here — the copy loop below runs to the end of the source either way, and
## would then publish the truncated item and delete the app-private original.
##
## The stored exception only lives until the next Java call, and formatting it is itself one
## (a JavaObject's string form goes through Java toString()), which is why the reference is taken
## before the string is built.
static func _java_reason() -> String:
	var ex := JavaClassWrapper.get_exception()
	return "" if ex == null else " — %s" % ex

static func _save(src_path: String, mime: String, is_video: bool) -> bool:
	if OS.get_name() != "Android":
		return false
	var src := FileAccess.open(src_path, FileAccess.READ)
	if src == null or src.get_length() == 0:
		push_warning("[demo-gallery] cannot read %s" % src_path)
		return false
	var name := src_path.get_file()
	var activity := XrealAndroidBridge.get_activity()
	var content_values_class := JavaClassWrapper.wrap("android.content.ContentValues")
	var media_class := JavaClassWrapper.wrap(
		"android.provider.MediaStore$Video$Media" if is_video
		else "android.provider.MediaStore$Images$Media")
	if activity == null or content_values_class == null or media_class == null:
		push_warning("[demo-gallery] Android runtime/classes unavailable")
		return false
	var rel_dir := "Movies/godot-xreal" if is_video else "Pictures/godot-xreal"
	var resolver = activity.getContentResolver()
	# Column names are the real MediaStore.MediaColumns constants, read straight off `media_class`:
	# JavaClassWrapper exposes a Java class's public static fields as properties, and Images$Media /
	# Video$Media inherit MediaColumns' through ImageColumns / VideoColumns. Only primitive and
	# String constants are exposed, though — the volume below stays a literal ("external_primary" is
	# the value of MediaStore.VOLUME_EXTERNAL_PRIMARY) and the collection Uri is still resolved with
	# getContentUri(), because a Uri-typed constant like EXTERNAL_CONTENT_URI is not reachable.
	var values = content_values_class.ContentValues()
	values.put(media_class.DISPLAY_NAME, name)
	values.put(media_class.MIME_TYPE, mime)
	values.put(media_class.RELATIVE_PATH, rel_dir)
	values.put(media_class.IS_PENDING, 1)
	var item = resolver.insert(media_class.getContentUri("external_primary"), values)
	var reason := _java_reason()
	if item == null:
		push_warning("[demo-gallery] MediaStore insert failed for %s%s" % [name, reason])
		return false
	# NB: JavaClassWrapper cannot pass null for String / String[] parameters ("Cannot convert
	# argument from Nil to String"), so the no-selection update/delete calls below use "" plus an
	# empty PackedStringArray instead — providers treat an empty selection like a null one.
	var no_where := ""
	var no_args := PackedStringArray()
	var out = resolver.openOutputStream(item)
	reason = _java_reason()
	if out == null:
		resolver.delete(item, no_where, no_args)
		push_warning("[demo-gallery] openOutputStream failed for %s%s" % [name, reason])
		return false
	while src.get_position() < src.get_length():
		out.write(src.get_buffer(_CHUNK))  # PackedByteArray -> byte[]
		# A throwing write (full volume, revoked Uri) does not stop the loop — get_buffer has already
		# advanced the source — so without this the half-written item would be published as whole.
		reason = _java_reason()
		if reason != "":
			out.close()
			resolver.delete(item, no_where, no_args)
			push_warning("[demo-gallery] write failed for %s%s" % [name, reason])
			return false
	out.flush()
	reason = _java_reason()
	out.close()
	if reason == "":
		reason = _java_reason()  # close() flushes too, and can fail on its own
	src.close()  # closed here, not left to scope: the source is deleted below
	if reason != "":
		resolver.delete(item, no_where, no_args)
		push_warning("[demo-gallery] could not finish writing %s%s" % [name, reason])
		return false
	values.clear()
	values.put(media_class.IS_PENDING, 0)
	# Clear IS_PENDING to publish the item — while it is pending, other apps (the gallery) can't
	# see it (it sits on disk as ".pending-<epoch>-<name>"). Verify the row really updated.
	var updated = resolver.update(item, values, no_where, no_args)
	reason = _java_reason()
	if updated == null or int(updated) < 1:
		resolver.delete(item, no_where, no_args)
		push_warning("[demo-gallery] IS_PENDING clear failed for %s%s — still hidden in the gallery"
			% [name, reason])
		return false
	print("[demo-gallery] saved -> %s/%s" % [rel_dir, name])
	# Published, so the app-private original is now a second copy nobody can see. Dropping it only
	# here — past every failure return above — is what makes a failed save non-destructive: the
	# capture stays in user:// and can be retried, which would not be true of a delete-then-verify.
	# A removal that fails is not a save failure: the item is already in the gallery, so warn and
	# still report success rather than have the caller think the capture was lost.
	var err := DirAccess.remove_absolute(src_path)
	if err != OK:
		push_warning("[demo-gallery] saved, but could not remove the original %s (err %d)"
			% [src_path, err])
	return true
