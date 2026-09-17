<script setup lang="ts">
import { Activity, CircleAlert, Gauge, Snowflake, Timer } from '@lucide/vue'
import { useId } from 'vue'

import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BasePopover from '@/components/base/BasePopover.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'

const enabled = defineModel<boolean>('enabled', { required: true })
const threshold = defineModel<string>('threshold', { required: true })
const windowSeconds = defineModel<string>('windowSeconds', { required: true })
const durationSeconds = defineModel<string>('durationSeconds', { required: true })
const probeEnabled = defineModel<boolean>('probeEnabled', { required: true })
const probeModel = defineModel<string>('probeModel', { required: true })
const adaptiveConcurrency = defineModel<boolean>('adaptiveConcurrency', { required: true })
const adaptiveConcurrencyHintId = useId()
</script>

<template>
  <BaseCard
    title="过载策略"
    description="上游繁忙或服务不可用时，使用过载策略保护"
  >
    <BaseForm class="max-w-6xl sm:grid-cols-2">
      <BaseSwitch
        v-model="enabled"
        class="col-span-full justify-self-start"
        label="启用策略"
        show-label
      />

      <BaseFormItem
        label="触发阈值"
        description="窗口内累计失败尝试达到此次数后触发"
      >
        <BaseInput
          v-model="threshold"
          :disabled="!enabled"
          aria-label="触发阈值"
          type="number"
          min="2"
          max="1000"
          step="1"
        >
          <template #prefix>
            <Activity class="size-4" />
          </template>
          <template #suffix>
            <span class="text-cp-sm">次</span>
          </template>
        </BaseInput>
      </BaseFormItem>

      <BaseFormItem
        label="统计窗口"
        description="每次失败后，计数有效期向后顺延"
      >
        <BaseInput
          v-model="windowSeconds"
          :disabled="!enabled"
          aria-label="统计窗口秒数"
          type="number"
          min="60"
          max="3600"
          step="1"
        >
          <template #prefix>
            <Timer class="size-4" />
          </template>
          <template #suffix>
            <span class="text-cp-sm">秒</span>
          </template>
        </BaseInput>
      </BaseFormItem>

      <BaseFormItem
        label="冷却时长"
        description="探测失败时，按此时长延后恢复"
      >
        <BaseInput
          v-model="durationSeconds"
          :disabled="!enabled"
          aria-label="冷却时长秒数"
          type="number"
          min="300"
          max="604800"
          step="1"
        >
          <template #prefix>
            <Snowflake class="size-4" />
          </template>
          <template #suffix>
            <span class="text-cp-sm">秒</span>
          </template>
        </BaseInput>
      </BaseFormItem>
      <BaseFormItem
        label="恢复探测模型"
        description="启用后需探测成功才恢复，模型留空时选择首个可用模型"
      >
        <template #extra>
          <BaseCheckbox
            v-model="probeEnabled"
            :disabled="!enabled"
            label="恢复前探测"
            show-label
          />
        </template>
        <BaseInput
          v-model="probeModel"
          :disabled="!enabled || !probeEnabled"
          aria-label="恢复探测模型"
          placeholder="留空自动选择"
        >
          <template #prefix>
            <Gauge class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>

      <div class="col-span-full flex items-center gap-1">
        <BaseCheckbox
          v-model="adaptiveConcurrency"
          :disabled="!enabled"
          label="自适应并发下调"
          show-label
        />
        <BasePopover class="-my-1" trigger="hover-click" placement="top-start" :hover-delay="240">
          <template #trigger="{ open }">
            <button
              type="button"
              class="inline-flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-cp-sm border-0 bg-transparent p-0 text-cp-text-tertiary outline-none transition-colors hover:text-cp-text focus-visible:ring-2 focus-visible:ring-cp-control-outline motion-reduce:transition-none"
              aria-label="自适应并发下调说明"
              :aria-expanded="open"
              :aria-describedby="open ? adaptiveConcurrencyHintId : undefined"
            >
              <CircleAlert class="size-3.5" aria-hidden="true" />
            </button>
          </template>
          <p :id="adaptiveConcurrencyHintId" role="tooltip" class="m-0 max-w-72 px-3 py-2 text-cp-sm leading-relaxed text-cp-text-secondary">
            修改账号并发上限，恢复后不自动调高
          </p>
        </BasePopover>
      </div>
    </BaseForm>
  </BaseCard>
</template>
