<script setup lang="ts">
import type {
  ApiKey,
  PluginCapabilityBinding,
  PluginContribution,
  PluginFailurePolicy,
  PluginFrontendIdentityBinding,
} from '@/api'

import { BaseButton, BaseFormItem, BaseIconButton, BaseInput, BaseSegmented, BaseSelect, BaseSwitch, toast } from '@codex-proxy/ui'

import { Plus, RefreshCw, Trash2 } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { getApiKeys } from '@/api'
import { errorMessage } from '@/utils/operation'
import PluginHelpPopover from './PluginHelpPopover.vue'

const props = withDefaults(defineProps<{
  contribution: PluginContribution
  active?: boolean
  disabled?: boolean
}>(), {
  active: false,
  disabled: false,
})

const emit = defineEmits<{
  validityChange: [valid: boolean]
}>()

const binding = defineModel<PluginCapabilityBinding | null>({ required: true })
const clientKeys = shallowRef<ApiKey[]>([])
const loadingKeys = shallowRef(false)
const keysFailed = shallowRef(false)
let keyLoadController: AbortController | null = null

const fallbackOptions = [
  { label: '独占认证', value: 'reject' },
  { label: '未匹配时回退 API Key', value: 'delegate' },
]

const keyOptions = computed(() => {
  const options = clientKeys.value.map(key => ({
    label: `${key.name || key.prefix}${key.enabled ? '' : '（已停用）'}`,
    value: key.id,
    description: `${key.prefix} · ${key.id}`,
  }))
  const knownIds = new Set(clientKeys.value.map(key => key.id))
  for (const identity of binding.value?.identityBindings ?? []) {
    if (identity.clientKeyId && !knownIds.has(identity.clientKeyId)) {
      options.push({
        label: '已配置但当前不可用',
        value: identity.clientKeyId,
        description: identity.clientKeyId,
      })
      knownIds.add(identity.clientKeyId)
    }
  }
  return options
})

const validationError = computed(() => {
  const value = binding.value
  if (!value)
    return ''
  if (
    value.contribution !== props.contribution.id
    || value.stage !== 'authentication'
    || !props.contribution.stages.includes(value.stage)
    || (value.failurePolicy !== 'reject' && value.failurePolicy !== 'delegate')
  ) {
    return '客户端认证绑定必须使用认证阶段和受支持的未匹配处理'
  }
  if (
    value.providerIds.length
    || value.models.length
    || value.clientKeyIds.length
    || value.accountGroupIds.length
  ) {
    return '客户端认证不支持 Provider、模型或分组范围，请关闭后重新启用该绑定'
  }
  if (value.identityBindings.length === 0)
    return '请添加至少一条身份映射'
  if (value.identityBindings.length > 256)
    return '身份映射不能超过 256 条'

  const principals = new Set<string>()
  for (const identity of value.identityBindings) {
    if (
      !identity.principal
      || identity.principal !== identity.principal.trim()
      || new TextEncoder().encode(identity.principal).byteLength > 256
      || /\p{Cc}/u.test(identity.principal)
    ) {
      return '身份标识必须是 1–256 个 UTF-8 字节，且不能含首尾空白或控制字符'
    }
    if (principals.has(identity.principal))
      return '同一身份标识只能配置一次'
    if (!identity.clientKeyId)
      return '请为每个身份标识选择客户端 Key'
    principals.add(identity.principal)
  }
  return ''
})

function enableAuthentication(enabled: boolean) {
  binding.value = enabled
    ? {
        contribution: props.contribution.id,
        stage: 'authentication',
        order: 0,
        failurePolicy: 'reject',
        providerIds: [],
        models: [],
        clientKeyIds: [],
        accountGroupIds: [],
        identityBindings: [{ principal: '', clientKeyId: '' }],
      }
    : null
}

function setFailurePolicy(value: string) {
  if (!binding.value || (value !== 'reject' && value !== 'delegate'))
    return
  binding.value = { ...binding.value, failurePolicy: value as PluginFailurePolicy }
}

function addIdentity() {
  if (!binding.value || binding.value.identityBindings.length >= 256)
    return
  binding.value = {
    ...binding.value,
    identityBindings: [
      ...binding.value.identityBindings,
      { principal: '', clientKeyId: '' },
    ],
  }
}

function updateIdentity(index: number, update: Partial<PluginFrontendIdentityBinding>) {
  if (!binding.value)
    return
  binding.value = {
    ...binding.value,
    identityBindings: binding.value.identityBindings.map((identity, identityIndex) =>
      identityIndex === index ? { ...identity, ...update } : identity,
    ),
  }
}

function removeIdentity(index: number) {
  if (!binding.value)
    return
  binding.value = {
    ...binding.value,
    identityBindings: binding.value.identityBindings.filter((_, identityIndex) => identityIndex !== index),
  }
}

async function loadClientKeys() {
  keyLoadController?.abort()
  const controller = new AbortController()
  keyLoadController = controller
  loadingKeys.value = true
  keysFailed.value = false
  try {
    const items: ApiKey[] = []
    const seenCursors = new Set<string>()
    let cursor: string | undefined
    do {
      const result = await getApiKeys({ limit: 200, cursor }, { signal: controller.signal, silent: true })
      items.push(...result.items)
      cursor = result.nextCursor ?? undefined
      if (cursor && seenCursors.has(cursor))
        throw new Error('服务返回了重复的分页位置')
      if (cursor)
        seenCursors.add(cursor)
    } while (cursor)
    if (!controller.signal.aborted)
      clientKeys.value = items
  }
  catch (error: unknown) {
    if (!controller.signal.aborted) {
      keysFailed.value = true
      toast.error(errorMessage(error, '加载客户端 Key 失败'))
    }
  }
  finally {
    if (keyLoadController === controller && !controller.signal.aborted) {
      loadingKeys.value = false
      keyLoadController = null
    }
  }
}

watch(validationError, error => emit('validityChange', !error), { immediate: true })
watch(() => props.active, (active, _, onCleanup) => {
  if (!active) {
    keyLoadController?.abort()
    keyLoadController = null
    return
  }
  void loadClientKeys()
  onCleanup(() => keyLoadController?.abort())
}, { immediate: true })

defineExpose({ validationError })
</script>

<template>
  <div class="grid gap-4">
    <div class="flex items-center gap-2 rounded-cp-lg bg-cp-fill-alter p-4">
      <BaseSwitch
        :model-value="Boolean(binding)"
        label="启用客户端认证"
        show-label
        :disabled="disabled"
        @update:model-value="enableAuthentication"
      />
      <PluginHelpPopover label="客户端认证说明">
        <p class="m-0">
          插件识别请求中的外部身份，宿主再将该身份映射到已有 Client Key，沿用这个 Key 的限额、账号范围与并发策略
        </p>
        <p class="m-0">
          此功能用于客户端 API 请求，不替代管理端登录
        </p>
        <p class="m-0 break-all font-mono text-cp-text-quaternary">
          {{ contribution.id }}
        </p>
      </PluginHelpPopover>
    </div>

    <template v-if="binding">
      <BaseFormItem label="未匹配处理">
        <template #label-extra>
          <PluginHelpPopover label="未匹配处理说明">
            <p class="m-0">
              独占认证：插件未识别到身份时拒绝请求
            </p>
            <p class="m-0">
              未匹配时回退 API Key：插件未识别到身份时，继续使用宿主原有的 API Key 认证
            </p>
            <p class="m-0">
              插件明确拒绝或调用失败时始终拒绝请求，不会回退
            </p>
          </PluginHelpPopover>
        </template>
        <BaseSegmented
          :model-value="binding.failurePolicy"
          :options="fallbackOptions"
          label="客户端认证未匹配处理"
          :disabled="disabled"
          @update:model-value="setFailurePolicy"
        />
      </BaseFormItem>

      <div class="grid gap-2">
        <div class="flex min-w-0 items-end justify-between gap-3">
          <div class="flex min-w-0 items-center gap-1.5">
            <p class="m-0 text-cp leading-none font-medium text-cp-text-secondary">
              身份映射
            </p>
            <PluginHelpPopover label="身份映射说明">
              <p class="m-0">
                身份标识必须与插件返回的值完全一致，同一身份只能映射一次
              </p>
              <p class="m-0">
                选择该身份使用的 Client Key，已停用或删除的 Key 不会获得执行权限
              </p>
            </PluginHelpPopover>
          </div>
          <div class="flex shrink-0 items-center gap-1">
            <BaseIconButton v-if="keysFailed" size="sm" label="重新加载客户端 Key" :disabled="disabled" :loading="loadingKeys" @click="loadClientKeys()">
              <RefreshCw class="size-3.5" />
            </BaseIconButton>
            <BaseButton
              size="sm"
              variant="secondary"
              :disabled="disabled || binding.identityBindings.length >= 256"
              @click="addIdentity"
            >
              <template #icon>
                <Plus class="size-3.5" />
              </template>
              添加映射
            </BaseButton>
          </div>
        </div>

        <div
          v-for="(identity, index) in binding.identityBindings"
          :key="index"
          class="grid min-w-0 gap-3 rounded-cp-lg bg-cp-fill-alter p-3 md:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] md:items-end"
        >
          <BaseFormItem label="身份标识" required>
            <BaseInput
              :model-value="identity.principal"
              maxlength="256"
              placeholder="填写插件返回的身份标识"
              :disabled="disabled"
              @update:model-value="updateIdentity(index, { principal: $event })"
            />
          </BaseFormItem>
          <BaseFormItem label="客户端 Key" required>
            <BaseSelect
              :model-value="identity.clientKeyId"
              :options="keyOptions"
              :disabled="disabled || loadingKeys"
              :placeholder="loadingKeys ? '加载中…' : keysFailed ? '加载失败，请重试' : '选择客户端 Key'"
              empty-text="暂无客户端 Key"
              class="w-full min-w-0"
              @update:model-value="updateIdentity(index, { clientKeyId: $event })"
            />
          </BaseFormItem>
          <BaseIconButton
            size="md"
            variant="ghost"
            :disabled="disabled"
            :label="`移除第 ${index + 1} 条身份映射`"
            class="justify-self-end"
            @click="removeIdentity(index)"
          >
            <Trash2 class="size-3.5" />
          </BaseIconButton>
        </div>
      </div>
    </template>
  </div>
</template>
