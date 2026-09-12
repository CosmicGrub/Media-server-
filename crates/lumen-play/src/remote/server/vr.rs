//! VR Stage 1: a single static "cinema" page, served over the same paired/TLS listener `http.rs`
//! already answers `/stream/<path>` on.
//!
//! **Why this needs no server-side logic at all.** The page never touches the filesystem and never
//! sees a token: it is the same bytes for every request, and does nothing without a valid `path` and
//! `token` in its own URL, at which point it just builds `/stream/<path>?token=<token>` -- the exact
//! URL `http.rs`'s own `handle_request` already authenticates and contains against the library root.
//! Serving the shell publicly adds no new privileged surface; the one place that ever checks a token
//! is unchanged. This is why `/vr` needs no `--flag` the way `--dlna` does: unlike DLNA's own listener,
//! nothing here is reachable, readable, or streamable without a token this server already issued.
//!
//! **Stage 1, honestly scoped.** One fixed flat screen floating in a plain dark void -- no rendered
//! room, no seat/screen-position choice, no library browsing (the caller already knows which file's
//! `path` to pass, the same one-file-at-a-time shape `/stream/<path>` itself has). No spatial audio:
//! the `<video>` element's normal stereo output plays exactly as it would in a flat browser tab. No
//! controller input beyond the page's own "Enter VR" button. Every one of these is a real limitation,
//! not a subtlety left implicit -- future VR stages are exactly "which of these gets built next."
//!
//! **Hand-rolled WebGL, no library.** Matches this workspace's dependency posture on the Rust side:
//! the render loop, shader program, and the one 4x4 matrix multiply it needs are all written out in
//! full below rather than pulling in three.js (or anything else) from a CDN this LAN device may have
//! no route to at all.

pub(super) const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>lumen — VR cinema</title>
<style>
  html, body { margin: 0; height: 100%; background: #0b0b0f; color: #ddd;
               font: 14px/1.4 system-ui, sans-serif; }
  #shell { display: flex; flex-direction: column; align-items: center; gap: 12px; padding: 24px; }
  video { width: 100%; max-width: 960px; background: #000; }
  button { font: inherit; padding: 8px 16px; cursor: pointer; }
  #status { color: #999; min-height: 1.2em; }
  canvas { display: none; }
</style>
</head>
<body>
<div id="shell">
  <video id="v" controls playsinline></video>
  <button id="enter" hidden>Enter VR</button>
  <div id="status"></div>
</div>
<canvas id="glcanvas"></canvas>
<script>
(function () {
  "use strict";

  // The library path and token travel exactly as they do for `/stream/<path>`: the caller already
  // built (or copied) that same URL's query string, so `path` here is read raw -- undecoded -- and
  // appended straight onto `/stream/`, rather than decoding and re-encoding it and risking a mismatch
  // with whatever encoding the caller actually used.
  function rawQueryParam(name) {
    const q = location.search.replace(/^\?/, "");
    for (const pair of q.split("&")) {
      const eq = pair.indexOf("=");
      if (eq === -1) continue;
      if (decodeURIComponent(pair.slice(0, eq)) === name) return pair.slice(eq + 1);
    }
    return null;
  }

  const status = document.getElementById("status");
  const video = document.getElementById("v");
  const enterBtn = document.getElementById("enter");

  const rawPath = rawQueryParam("path");
  const rawToken = rawQueryParam("token");
  if (!rawPath || !rawToken) {
    status.textContent = "missing ?path=&token= -- this page is not meant to be opened by hand.";
    return;
  }
  video.src = "/stream/" + rawPath + "?token=" + rawToken;

  if (!("xr" in navigator)) {
    status.textContent = "WebXR is not available in this browser; playing in 2D.";
    return;
  }
  navigator.xr.isSessionSupported("immersive-vr").then(function (supported) {
    if (supported) {
      enterBtn.hidden = false;
    } else {
      status.textContent = "no immersive-vr headset detected; playing in 2D.";
    }
  }, function () {
    status.textContent = "could not query WebXR support; playing in 2D.";
  });

  // A screen 4m wide, 2.25m tall (16:9), centred 3.5m in front of the origin at seated eye height --
  // fixed world-space vertex positions, so no per-frame model matrix is needed, only the projection
  // and view matrices WebXR already hands back each frame.
  const SCREEN = new Float32Array([
    // x,     y,     z,     u, v
    -2.0,  0.475, -3.5,   0.0, 1.0,
     2.0,  0.475, -3.5,   1.0, 1.0,
     2.0,  2.725, -3.5,   1.0, 0.0,
    -2.0,  0.475, -3.5,   0.0, 1.0,
     2.0,  2.725, -3.5,   1.0, 0.0,
    -2.0,  2.725, -3.5,   0.0, 0.0,
  ]);

  const VERTEX_SRC =
    "attribute vec3 aPos; attribute vec2 aUv; uniform mat4 uMvp; varying vec2 vUv;" +
    "void main() { vUv = aUv; gl_Position = uMvp * vec4(aPos, 1.0); }";
  const FRAGMENT_SRC =
    "precision mediump float; varying vec2 vUv; uniform sampler2D uTex;" +
    "void main() { gl_FragColor = texture2D(uTex, vUv); }";

  function compile(gl, type, source) {
    const s = gl.createShader(type);
    gl.shaderSource(s, source);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
      throw new Error("shader compile failed: " + gl.getShaderInfoLog(s));
    }
    return s;
  }

  // Column-major 4x4 multiply, `a * b`, matching the layout every matrix WebXR itself hands back.
  function mat4Multiply(a, b) {
    const out = new Float32Array(16);
    for (let col = 0; col < 4; col++) {
      for (let row = 0; row < 4; row++) {
        let sum = 0;
        for (let k = 0; k < 4; k++) sum += a[k * 4 + row] * b[col * 4 + k];
        out[col * 4 + row] = sum;
      }
    }
    return out;
  }

  let session = null;
  let gl = null;
  let program = null;
  let uMvpLoc = null;
  let vertexBuffer = null;
  let videoTexture = null;
  let refSpace = null;

  function setupGl() {
    const canvas = document.getElementById("glcanvas");
    gl = canvas.getContext("webgl", { xrCompatible: true });
    const vs = compile(gl, gl.VERTEX_SHADER, VERTEX_SRC);
    const fs = compile(gl, gl.FRAGMENT_SHADER, FRAGMENT_SRC);
    program = gl.createProgram();
    gl.attachShader(program, vs);
    gl.attachShader(program, fs);
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      throw new Error("program link failed: " + gl.getProgramInfoLog(program));
    }
    uMvpLoc = gl.getUniformLocation(program, "uMvp");

    vertexBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, vertexBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, SCREEN, gl.STATIC_DRAW);

    videoTexture = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, videoTexture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, true);
    gl.enable(gl.DEPTH_TEST);
    return canvas;
  }

  function onXrFrame(time, frame) {
    session.requestAnimationFrame(onXrFrame);
    const pose = frame.getViewerPose(refSpace);
    if (!pose) return;

    const layer = session.renderState.baseLayer;
    gl.bindFramebuffer(gl.FRAMEBUFFER, layer.framebuffer);
    gl.clearColor(0.02, 0.02, 0.03, 1.0);
    gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);

    if (video.readyState >= video.HAVE_CURRENT_DATA) {
      gl.bindTexture(gl.TEXTURE_2D, videoTexture);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, video);
    }

    gl.useProgram(program);
    gl.bindBuffer(gl.ARRAY_BUFFER, vertexBuffer);
    const aPos = gl.getAttribLocation(program, "aPos");
    const aUv = gl.getAttribLocation(program, "aUv");
    gl.enableVertexAttribArray(aPos);
    gl.vertexAttribPointer(aPos, 3, gl.FLOAT, false, 20, 0);
    gl.enableVertexAttribArray(aUv);
    gl.vertexAttribPointer(aUv, 2, gl.FLOAT, false, 20, 12);
    gl.uniform1i(gl.getUniformLocation(program, "uTex"), 0);

    for (const view of pose.views) {
      const vp = layer.getViewport(view);
      gl.viewport(vp.x, vp.y, vp.width, vp.height);
      const mvp = mat4Multiply(view.projectionMatrix, view.transform.inverse.matrix);
      gl.uniformMatrix4fv(uMvpLoc, false, mvp);
      gl.drawArrays(gl.TRIANGLES, 0, 6);
    }
  }

  enterBtn.addEventListener("click", async function () {
    try {
      await video.play();
      const canvas = setupGl();
      await gl.makeXRCompatible();
      session = await navigator.xr.requestSession("immersive-vr");
      session.updateRenderState({ baseLayer: new XRWebGLLayer(session, gl) });
      try {
        refSpace = await session.requestReferenceSpace("local-floor");
      } catch (e) {
        refSpace = await session.requestReferenceSpace("local");
      }
      session.addEventListener("end", function () {
        session = null;
        status.textContent = "left VR.";
      });
      status.textContent = "in VR — take off the headset or use its menu to exit.";
      session.requestAnimationFrame(onXrFrame);
    } catch (e) {
      status.textContent = "could not start VR: " + e.message;
    }
  });
})();
</script>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_requests_an_immersive_vr_session_and_streams_via_the_existing_auth() {
        assert!(PAGE.contains(r#"navigator.xr.requestSession("immersive-vr")"#));
        // The page must build its stream URL from `/stream/`, `path`, and `token` -- never invent a
        // second, unauthenticated path to the same bytes `http.rs`'s own handler already guards.
        assert!(PAGE.contains(r#""/stream/" + rawPath + "?token=" + rawToken"#));
    }

    #[test]
    fn the_page_falls_back_honestly_when_webxr_or_a_headset_is_missing() {
        assert!(PAGE.contains("WebXR is not available"));
        assert!(PAGE.contains("no immersive-vr headset detected"));
    }
}
