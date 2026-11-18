<template>
  <div class="mt-login-success" aria-live="polite">
    <div class="mt-login-success-mark" aria-hidden="true">✓</div>
    <h2>身份已确认</h2>
    <p>正在打开控制平面。会话将在浏览器中保持 HttpOnly 保护。</p>
    <div class="mt-login-success-bar"><i></i></div>
  </div>
</template>

<script setup lang="ts">
defineOptions({ name: 'LoginSuccessState' })
</script>
