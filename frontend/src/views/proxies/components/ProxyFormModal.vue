<script setup lang="ts">
import type { OutboundProxyRecord, OutboundProxyTest, RequestLocation } from '@/api'
import { BaseButton, BaseForm, BaseFormItem, BaseIconButton, BaseInput, BaseModal, BaseSwitch } from '@codex-proxy/ui'
import { Eye, EyeOff, LocateFixed, Save, Wifi } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import RequestLocationFields from '@/components/RequestLocationFields.vue'

const props = defineProps<{
  proxy: OutboundProxyRecord | null
  saving: boolean
  testingConnection: boolean
  detectingLocation: boolean
  testResult: OutboundProxyTest | null
}>()
const emit = defineEmits<{
  save: []
  test: []
  detectLocation: []
}>()
const open = defineModel<boolean>({ required: true })
const name = defineModel<string>('name', { required: true })
const proxyUrl = defineModel<string>('proxyUrl', { required: true })
const customLocation = defineModel<boolean>('customLocation', { required: true })
const location = defineModel<RequestLocation>('location', { required: true })
const manualLocationWarning = computed(() => {
  const location = props.testResult?.location
  if (location?.status === 'conflict')
    return 'IPv4 与 IPv6 出口时区不一致，请手动填写'
  if (location?.status === 'failed')
    return location.message
  return ''
})
const showSecret = shallowRef(false)
const busy = computed(() => props.saving || props.testingConnection || props.detectingLocation)
const title = computed(() => props.proxy ? '编辑代理' : '新增代理')
const connectionDescription = computed(() => props.proxy
  ? '留空保留当前连接和认证信息，填写新地址时，请包含所需的用户名和密码'
  : '支持 HTTP、HTTPS、SOCKS5 和 SOCKS5H，可在地址中包含用户名和密码')

watch(open, (value) => {
  if (value)
    showSecret.value = false
})
</script>

<template>
  <BaseModal v-model="open" :title="title" size="md" :dismissible="!busy">
    <BaseForm class="grid gap-5">
      <BaseFormItem label="代理名称" required>
        <BaseInput v-model="name" maxlength="100" :disabled="busy" aria-label="代理名称" placeholder="请输入代理名称" />
      </BaseFormItem>
      <BaseFormItem label="代理地址" :required="!proxy" :description="connectionDescription">
        <BaseInput
          v-model="proxyUrl"
          :type="showSecret ? 'text' : 'password'"
          autocomplete="new-password"
          :disabled="busy"
          aria-label="代理地址"
          placeholder="请输入代理地址"
        >
          <template #suffix>
            <BaseIconButton :label="showSecret ? '隐藏代理地址' : '显示代理地址'" :disabled="busy" @click="showSecret = !showSecret">
              <EyeOff v-if="showSecret" class="size-4" />
              <Eye v-else class="size-4" />
            </BaseIconButton>
          </template>
        </BaseInput>
      </BaseFormItem>
      <div class="flex items-center justify-between gap-3">
        <BaseSwitch v-model="customLocation" label="自定义时区位置" show-label :disabled="busy" />
        <BaseIconButton
          size="md"
          :label="detectingLocation ? '正在解析出口位置' : '解析出口位置并填入表单'"
          :title="detectingLocation ? '正在解析出口位置' : '解析出口位置并填入表单'"
          :aria-busy="detectingLocation"
          :disabled="busy"
          @click="emit('detectLocation')"
        >
          <span class="relative grid size-7 place-items-center text-cp-link">
            <span v-if="detectingLocation" class="location-ripple" aria-hidden="true" />
            <span v-if="detectingLocation" class="location-ripple location-ripple-delayed" aria-hidden="true" />
            <LocateFixed class="relative z-10 size-4" aria-hidden="true" />
          </span>
        </BaseIconButton>
      </div>
      <p v-if="testResult?.success === false" class="m-0 text-cp-sm text-cp-error-text" role="alert">
        连接测试失败：{{ testResult.message }}
      </p>
      <p v-else-if="manualLocationWarning" class="m-0 text-cp-sm text-cp-warning-text" role="status">
        {{ manualLocationWarning }}
      </p>
      <RequestLocationFields v-if="customLocation" v-model="location" :disabled="busy" />
      <p v-if="proxy?.accountCount && proxyUrl.trim()" class="m-0 text-cp-sm text-cp-warning-text">
        将更新 {{ proxy.accountCount }} 个关联账号的出口
      </p>
    </BaseForm>
    <template #footer>
      <BaseButton variant="secondary" :disabled="busy" @click="open = false">
        取消
      </BaseButton>
      <BaseButton variant="secondary" :loading="testingConnection" :disabled="saving || detectingLocation" @click="emit('test')">
        <template #icon>
          <Wifi class="size-4" />
        </template>
        测试连接
      </BaseButton>
      <BaseButton variant="primary" :loading="saving" :disabled="testingConnection || detectingLocation" @click="emit('save')">
        <template #icon>
          <Save class="size-4" />
        </template>
        保存代理
      </BaseButton>
    </template>
  </BaseModal>
</template>

<style scoped>
.location-ripple {
  position: absolute;
  width: 1.25rem;
  height: 1.25rem;
  border: 1px solid currentColor;
  border-radius: 50%;
  opacity: 0;
  pointer-events: none;
  animation: location-ripple 1.8s ease-out infinite forwards;
}

.location-ripple-delayed {
  animation-delay: 0.9s;
}

@keyframes location-ripple {
  from {
    opacity: 0.3;
    transform: scale(0.6);
  }

  to {
    opacity: 0;
    transform: scale(1.55);
  }
}

@media (prefers-reduced-motion: reduce) {
  .location-ripple {
    animation: none;
    opacity: 0.4;
    transform: scale(1.1);
  }

  .location-ripple-delayed {
    display: none;
  }
}
</style>
