import { useCallback } from 'react'
import { previewRequesterMove, type MovePreview } from './api/client'
import { useLoad, type Loaded } from './useAction'

/**
 * What moving the requester to `customerId` would do: the tickets that move along, the site they
 * leave and the links removed. Nothing is loaded until both are chosen.
 */
export function useMovePreview(
  requesterId: string | null,
  customerId: string,
  onSignedOut: () => void,
): Loaded<MovePreview | null> {
  const load = useCallback(
    (): Promise<MovePreview | null> =>
      (requesterId && customerId ? previewRequesterMove(requesterId, customerId) : Promise.resolve(null)),
    [requesterId, customerId])
  return useLoad(load, onSignedOut)
}
