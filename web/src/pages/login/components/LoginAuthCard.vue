<template>
  <aside class="mt-login-auth" :class="{ 'is-success': success }" aria-labelledby="mtLoginTitle">
    <LoginAuthHeader />

    <form v-if="!success" class="mt-login-form" novalidate @submit.prevent="onSubmit">
      <LoginTokenField
        :model-value="token"
        :disabled="busy"
        :invalid="Boolean(errorText)"
        :described-by="errorText ? 'mtLoginTokenHint mtLoginError' : 'mtLoginTokenHint'"
        @update:model-value="onTokenInput"
      />
      <LoginErrorAlert v-if="errorText" :text="errorText" />
      <LoginSubmitButton :busy="busy" />
    </form>

    <LoginSuccessState v-else />
    <LoginAuthFoot />
    <LoginSignalRow />
  </aside>
</template>

<script setup lang="ts">
import { nextTick, ref } from 'vue'
import { useRouter } from 'vue-router'
import { login, refreshSession, session } from '../../../services/api'
import LoginAuthFoot from './LoginAuthFoot.vue'
import LoginAuthHeader from './LoginAuthHeader.vue'
import LoginErrorAlert from './LoginErrorAlert.vue'
import LoginSignalRow from './LoginSignalRow.vue'
import LoginSubmitButton from './LoginSubmitButton.vue'
import LoginSuccessState from './LoginSuccessState.vue'
import LoginTokenField from './LoginTokenField.vue'

defineOptions({ name: 'LoginAuthCard' })

const router = useRouter()
const token = ref('')
const busy = ref(false)
const success = ref(false)
const errorText = ref('')

function focusToken() {
  document.getElementById('mtLoginToken')?.focus()
}

function onTokenInput(value: string) {
  token.value = value
  errorText.value = ''
}

async function onSubmit() {
  if (busy.value) return
  errorText.value = ''
  if (!token.value.trim()) {
    errorText.value = '请输入 admin_token。'
    await nextTick()
    focusToken()
    return
  }

  busy.value = true
  try {
    await login(token.value)
    success.value = true
    window.setTimeout(() => {
      void router.replace('/')
    }, 700)
  } catch (err) {
    const message = err instanceof Error ? err.message : '登录失败'
    if (message.includes('too many')) {
      errorText.value = '失败次数过多，请 10 分钟后再试。'
    } else if (message.includes('invalid')) {
      errorText.value = 'Token 校验未通过。请检查 /data/admin.token 后重试。'
    } else {
      errorText.value = message
    }
    token.value = ''
    session.authenticated = false
    await nextTick()
    focusToken()
  } finally {
    busy.value = false
  }
}

// 已登录时刷新会话，避免登录页重复输入。
refreshSession().then(async (snapshot) => {
  if (snapshot.authenticated) await router.replace('/')
})
</script>
