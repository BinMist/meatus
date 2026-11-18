<template>
  <section class="mt-login-stage" aria-labelledby="mtLoginHeadline">
    <LoginHud />
    <div class="mt-login-stage-body">
      <LoginHeroCopy />
      <LoginFlowDiagram />
      <LoginTelemetry />
    </div>
    <LoginBaseline />
  </section>
</template>

<script setup lang="ts">
import LoginBaseline from './LoginBaseline.vue'
import LoginFlowDiagram from './LoginFlowDiagram.vue'
import LoginHeroCopy from './LoginHeroCopy.vue'
import LoginHud from './LoginHud.vue'
import LoginTelemetry from './LoginTelemetry.vue'

defineOptions({ name: 'LoginStage' })
</script>
