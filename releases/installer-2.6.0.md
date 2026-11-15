OCBM 2.6.0 installer image (A15W_Update.img): converts a stock Carlinkit CPC200-CCPA (A15W, firmware 2025.10.15.1127) to OCBM 2.6.0 from a USB stick, or from Carlink. Built from enigy/ocbm <COMMIT> ("2.6.0 ocbm <COMMIT>"), with 2.0.0's builds of the unchanged iap2d and iap_role_switch; its files match the update package on release v2.6.0, which says what changed since 2.5.0.

## Install OCBM from a USB stick (a stock adapter)

The adapter must be a Carlinkit CPC200-CCPA (A15W) on the stock 2025.10.15.1127 firmware.

1. Format a USB stick as FAT32 (at least 64 MB free; the install writes a full backup of the adapter's flash to it).
2. Copy A15W_Update.img to the root of the stick. Check it against A15W_Update.img.sha256 if you can.
3. Plug the stick into the adapter, then power the adapter (turn the car on, or plug the adapter in). Leave it alone for about 2 minutes.
4. The adapter restarts on OCBM. OCBM_RESULT.txt on the stick says INSTALLED, or REFUSED or STOPPED and why.
5. Take the stick out and keep it: OCBM_BACKUP on it is the adapter's factory flash, and the only copy. It holds the adapter's Wi-Fi password and Bluetooth keys, so keep it private.

Carlink can also write this image to a stick in the adapter for you. Stock firmware stays on the adapter as a failsafe until you remove it in Carlink (Settings > Adapter and about).

Going back to stock from a finished adapter (stock firmware removed) wipes its paired phones: forget the adapter (CarLink-xxxx) on each phone first, and pair them again afterwards.

The image also updates an adapter already on any OCBM from 1.0.1 to 2.5.0, if it has room. More: MIGRATING.md and docs/ops/01_RECOVERY.md.

No install is guaranteed to succeed. Until you remove the stock firmware, the adapter goes back to it by itself if OCBM fails its start-up self-test.

## Third-party software in this image

carplayd, the CarPlay receiver in this image, includes the Fraunhofer FDK AAC Codec Library for Android (fdk-aac 2.0.3, unmodified), statically linked. It encodes the microphone audio sent to the iPhone during wireless CarPlay (AAC-ELD).

- Licence: Software License for The Fraunhofer FDK AAC Codec Library for Android, in full below.
- Source code: the complete source of this version is available free of charge at https://github.com/mstorsjo/fdk-aac/tree/v2.0.3, and attached to this release as fdk-aac-2.0.3.tar.gz (a git archive of tag v2.0.3).
- Patents: the licence grants no patent rights.

<details>
<summary>Software License for The Fraunhofer FDK AAC Codec Library for Android</summary>

<pre>
Software License for The Fraunhofer FDK AAC Codec Library for Android

© Copyright  1995 - 2018 Fraunhofer-Gesellschaft zur Förderung der angewandten
Forschung e.V. All rights reserved.

 1.    INTRODUCTION
The Fraunhofer FDK AAC Codec Library for Android ("FDK AAC Codec") is software
that implements the MPEG Advanced Audio Coding ("AAC") encoding and decoding
scheme for digital audio. This FDK AAC Codec software is intended to be used on
a wide variety of Android devices.

AAC's HE-AAC and HE-AAC v2 versions are regarded as today's most efficient
general perceptual audio codecs. AAC-ELD is considered the best-performing
full-bandwidth communications codec by independent studies and is widely
deployed. AAC has been standardized by ISO and IEC as part of the MPEG
specifications.

Patent licenses for necessary patent claims for the FDK AAC Codec (including
those of Fraunhofer) may be obtained through Via Licensing
(www.vialicensing.com) or through the respective patent owners individually for
the purpose of encoding or decoding bit streams in products that are compliant
with the ISO/IEC MPEG audio standards. Please note that most manufacturers of
Android devices already license these patent claims through Via Licensing or
directly from the patent owners, and therefore FDK AAC Codec software may
already be covered under those patent licenses when it is used for those
licensed purposes only.

Commercially-licensed AAC software libraries, including floating-point versions
with enhanced sound quality, are also available from Fraunhofer. Users are
encouraged to check the Fraunhofer website for additional applications
information and documentation.

2.    COPYRIGHT LICENSE

Redistribution and use in source and binary forms, with or without modification,
are permitted without payment of copyright license fees provided that you
satisfy the following conditions:

You must retain the complete text of this software license in redistributions of
the FDK AAC Codec or your modifications thereto in source code form.

You must retain the complete text of this software license in the documentation
and/or other materials provided with redistributions of the FDK AAC Codec or
your modifications thereto in binary form. You must make available free of
charge copies of the complete source code of the FDK AAC Codec and your
modifications thereto to recipients of copies in binary form.

The name of Fraunhofer may not be used to endorse or promote products derived
from this library without prior written permission.

You may not charge copyright license fees for anyone to use, copy or distribute
the FDK AAC Codec software or your modifications thereto.

Your modified versions of the FDK AAC Codec must carry prominent notices stating
that you changed the software and the date of any change. For modified versions
of the FDK AAC Codec, the term "Fraunhofer FDK AAC Codec Library for Android"
must be replaced by the term "Third-Party Modified Version of the Fraunhofer FDK
AAC Codec Library for Android."

3.    NO PATENT LICENSE

NO EXPRESS OR IMPLIED LICENSES TO ANY PATENT CLAIMS, including without
limitation the patents of Fraunhofer, ARE GRANTED BY THIS SOFTWARE LICENSE.
Fraunhofer provides no warranty of patent non-infringement with respect to this
software.

You may use this FDK AAC Codec software or modifications thereto only for
purposes that are authorized by appropriate patent licenses.

4.    DISCLAIMER

This FDK AAC Codec software is provided by Fraunhofer on behalf of the copyright
holders and contributors "AS IS" and WITHOUT ANY EXPRESS OR IMPLIED WARRANTIES,
including but not limited to the implied warranties of merchantability and
fitness for a particular purpose. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR
CONTRIBUTORS BE LIABLE for any direct, indirect, incidental, special, exemplary,
or consequential damages, including but not limited to procurement of substitute
goods or services; loss of use, data, or profits, or business interruption,
however caused and on any theory of liability, whether in contract, strict
liability, or tort (including negligence), arising in any way out of the use of
this software, even if advised of the possibility of such damage.

5.    CONTACT INFORMATION

Fraunhofer Institute for Integrated Circuits IIS
Attention: Audio and Multimedia Departments - FDK AAC LL
Am Wolfsmantel 33
91058 Erlangen, Germany

www.iis.fraunhofer.de/amm
amm-info@iis.fraunhofer.de

</pre>
</details>



