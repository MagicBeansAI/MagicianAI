// Offscreen document for tab capture frame extraction.
// Receives streamIds from background, captures video frames via canvas,
// and sends base64 JPEG frames back to the background service worker.

// State
let activeCaptures = new Map(); // tabId -> { stream, interval, video, canvas, ctx }

// Listen for messages from background
chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    if (message.target !== 'offscreen') return;

    if (message.action === 'startCapture') {
        startCapture(message.streamId, message.tabId, message.fps || 15, message.quality || 0.50, message.width || 960, message.height || 540);
        sendResponse({ success: true });
    } else if (message.action === 'stopCapture') {
        stopCapture(message.tabId);
        sendResponse({ success: true });
    } else if (message.action === 'stopAll') {
        stopAllCaptures();
        sendResponse({ success: true });
    }
    return true; // async response
});

async function startCapture(streamId, tabId, fps, quality, width, height) {
    // Stop existing capture for this tab if any
    stopCapture(tabId);

    let stream = null;
    try {
        stream = await navigator.mediaDevices.getUserMedia({
            audio: false,
            video: {
                mandatory: {
                    chromeMediaSource: 'tab',
                    chromeMediaSourceId: streamId
                }
            }
        });

        // Create dedicated elements for this capture so concurrent
        // captures each draw from their own stream.
        const video = document.createElement('video');
        video.autoplay = true;
        video.muted = true;
        video.playsInline = true;

        const canvas = document.createElement('canvas');
        canvas.width = width;
        canvas.height = height;
        const ctx = canvas.getContext('2d');

        // Register onended BEFORE play() to avoid missing early track-end events.
        const videoTrack = stream.getVideoTracks()[0];
        if (videoTrack) {
            videoTrack.onended = () => {
                stopCapture(tabId);
                chrome.runtime.sendMessage({
                    target: 'background',
                    type: 'offscreen_capture_ended',
                    tabId: tabId
                });
            };
        }

        video.srcObject = stream;
        await video.play();

        const intervalMs = Math.floor(1000 / fps);
        const interval = setInterval(() => {
            if (video.readyState < video.HAVE_CURRENT_DATA) return;

            ctx.drawImage(video, 0, 0, width, height);
            const dataUrl = canvas.toDataURL('image/jpeg', quality);

            // Skip if frame is too large (> 500KB base64)
            if (dataUrl.length > 500 * 1024) return;

            // Extract just the base64 part (remove "data:image/jpeg;base64," prefix)
            const [header, base64Data] = dataUrl.split(',');
            if (!base64Data) return;
            const mimeType = /^data:([^;]+);base64$/.exec(header || '')?.[1] || 'image/jpeg';

            chrome.runtime.sendMessage({
                target: 'background',
                type: 'offscreen_frame',
                tabId: tabId,
                data: base64Data,
                mimeType,
                timestamp: Date.now(),
                metadata: { captureMode: 'tab_capture' }
            });
        }, intervalMs);

        activeCaptures.set(tabId, { stream, interval, video, canvas, ctx });

    } catch (error) {
        // Release the stream if acquired before the failure (e.g., video.play() rejected)
        if (stream) {
            stream.getTracks().forEach(track => track.stop());
        }
        console.error('[Offscreen] Failed to start capture:', error);
        chrome.runtime.sendMessage({
            target: 'background',
            type: 'offscreen_capture_error',
            tabId: tabId,
            error: error.message
        });
    }
}

function stopCapture(tabId) {
    const capture = activeCaptures.get(tabId);
    if (!capture) return;

    clearInterval(capture.interval);
    capture.stream.getTracks().forEach(track => track.stop());
    capture.video.srcObject = null;
    activeCaptures.delete(tabId);
}

function stopAllCaptures() {
    const tabIds = [...activeCaptures.keys()];
    for (const tabId of tabIds) {
        try {
            stopCapture(tabId);
        } catch (e) {
            console.warn('[Offscreen] Error stopping capture for tab', tabId, e);
        }
    }
}
