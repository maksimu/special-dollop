/*
 * FreeRDP header wrapper for bindgen
 *
 * This file includes the FreeRDP headers needed for RDP client functionality.
 * It's used by bindgen to generate Rust FFI bindings.
 */

#include <freerdp/freerdp.h>
#include <freerdp/client.h>
#include <freerdp/gdi/gdi.h>
#include <freerdp/gdi/gfx.h>
#include <freerdp/channels/channels.h>
#include <freerdp/client/channels.h>
#include <freerdp/client/cliprdr.h>
#include <freerdp/client/rdpgfx.h>
#include <freerdp/client/disp.h>
#include <freerdp/input.h>
#include <freerdp/update.h>
#include <freerdp/settings.h>
#include <freerdp/graphics.h>
#include <freerdp/codec/color.h>
#include <freerdp/codec/bitmap.h>

#include <winpr/crt.h>
#include <winpr/synch.h>
#include <winpr/thread.h>
#include <winpr/stream.h>
#include <winpr/wtypes.h>
