<template>
  <header class="mt-login-hud">
    <div class="mt-login-brand">
      <img src="/brand/meatus-icon-64.png" alt="Meatus" width="38" height="38" />
      <div class="mt-login-brand-name">
        <b>MEATUS</b>
        <span>CONTROL PLANE</span>
      </div>
    </div>
    <div class="mt-login-hud-right">
      <span class="mt-login-live"><i></i> SYSTEM READY</span>
      <span class="mt-login-coords">EDGE / LOOPBACK / TLS</span>
    </div>
  </header>
</template>

<script setup lang="ts">
defineOptions({ name: 'LoginHud' })
</script>
