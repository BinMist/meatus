<template>
  <div class="mt-login-screen">
    <main class="mt-login-layout">
      <LoginStage />
      <LoginAuthCard />
    </main>
  </div>
</template>

<script setup lang="ts">
import './styles.scss'
import LoginAuthCard from './components/LoginAuthCard.vue'
import LoginStage from './components/LoginStage.vue'
</script>
