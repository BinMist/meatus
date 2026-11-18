<template>
  <div class="mt-login-auth-head">
    <div>
      <div class="mt-login-kicker">AUTH GATE / 管理入口</div>
      <div class="mt-login-auth-title" id="mtLoginTitle">进入控制平面</div>
      <div class="mt-login-auth-sub">使用部署时生成的 admin_token 验证身份</div>
    </div>
    <sl-status-tag class="mode-chip" status="success" size="sm" label="LOOPBACK" pulse />
  </div>
</template>

<script setup lang="ts">
import { SlStatusTag } from '@sailonix/ui/status-tag'

defineOptions({ name: 'LoginAuthHeader' })
</script>
