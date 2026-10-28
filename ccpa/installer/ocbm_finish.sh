#!/bin/sh
# OCBM_STAGE_C_PLACEHOLDER
# ocbm_finish.sh — Stage C, not written yet (milestone M4): after the first Carlink3 session, remove
# the Carlinkit stack and set /script/ocbm_mode to final. Nothing calls it before M4, so an adapter
# installed with this image stays in its trial with stock kept as the fallback.
# tools/make_ocbm_installer.sh marks an image built with this placeholder as a bench build.
echo "[finish] Stage C is not written yet (M4): the adapter stays in its trial" >&2
exit 1
