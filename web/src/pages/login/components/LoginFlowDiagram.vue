<template>
  <div class="mt-login-visual">
    <svg class="mt-login-map" viewBox="0 0 760 306" role="img" aria-label="客户端、Meatus 边缘节点和目标服务之间的私有链路">
      <defs>
        <radialGradient id="mtLoginHalo" cx="50%" cy="50%" r="50%">
          <stop offset="0" stop-color="#236de9" stop-opacity=".18" />
          <stop offset=".62" stop-color="#236de9" stop-opacity=".04" />
          <stop offset="1" stop-color="#236de9" stop-opacity="0" />
        </radialGradient>
      </defs>
      <circle cx="380" cy="150" r="132" fill="url(#mtLoginHalo)" />
      <circle cx="380" cy="150" r="84" class="mt-login-pulse" />
      <circle cx="380" cy="150" r="84" class="mt-login-pulse mt-login-d2" />
      <circle cx="380" cy="150" r="84" class="mt-login-pulse mt-login-d3" />

      <line x1="152" y1="150" x2="608" y2="150" stroke="rgba(35,109,233,.14)" />
      <line x1="152" y1="150" x2="608" y2="150" class="mt-login-beam" />
      <line x1="152" y1="150" x2="608" y2="150" class="mt-login-beam mt-login-beam-2" />

      <g>
        <circle cx="102" cy="150" r="34" fill="none" stroke="rgba(46,58,62,.2)" />
        <circle cx="102" cy="150" r="5" fill="#236de9" opacity=".8" />
        <path d="M88 139h28M88 150h28M88 161h28" stroke="rgba(46,58,62,.18)" />
        <text x="102" y="208" text-anchor="middle">CLIENT</text>
        <text x="102" y="223" text-anchor="middle" class="mt-login-dim">HOME-NAS</text>
      </g>

      <g>
        <path d="M314 106h132v16H464v88H330v-88h-16z" fill="none" stroke="rgba(35,109,233,.18)" />
        <circle cx="380" cy="150" r="53" fill="rgba(255,255,255,.96)" stroke="rgba(35,109,233,.5)" />
        <circle cx="380" cy="150" r="63" class="mt-login-node-ring" />
        <image href="/brand/meatus-icon-64.png" x="354" y="124" width="52" height="52" />
        <path d="M334 116h18M334 184h18M445 116h-18M445 184h-18" stroke="#236de9" stroke-width="1" opacity=".35" />
        <text x="380" y="226" text-anchor="middle" class="mt-login-hot">MEATUS EDGE</text>
        <text x="380" y="241" text-anchor="middle" class="mt-login-dim">TLS / YAMUX</text>
      </g>

      <g>
        <circle cx="658" cy="150" r="34" fill="none" stroke="rgba(46,58,62,.2)" />
        <circle cx="658" cy="150" r="5" fill="#236de9" opacity=".8" />
        <path d="M644 139h28M644 150h28M644 161h28" stroke="rgba(46,58,62,.18)" />
        <text x="658" y="208" text-anchor="middle">TARGET</text>
        <text x="658" y="223" text-anchor="middle" class="mt-login-dim">PRIVATE SERVICE</text>
      </g>

      <g opacity=".5">
        <path d="M56 254h648" stroke="rgba(46,58,62,.13)" />
        <path d="M56 248v12M110 251v6M164 248v12M218 251v6M272 248v12M326 251v6M380 248v12M434 251v6M488 248v12M542 251v6M596 248v12M650 251v6M704 248v12" stroke="rgba(46,58,62,.2)" />
      </g>
    </svg>
    <div class="mt-login-visual-caption">
      <span><i></i> CLIENT → EDGE</span>
      <span><i></i> TLS 1.3</span>
      <span><i></i> LOOPBACK BIND</span>
    </div>
  </div>
</template>

<script setup lang="ts">
defineOptions({ name: 'LoginFlowDiagram' })
</script>
