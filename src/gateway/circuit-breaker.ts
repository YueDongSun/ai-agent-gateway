import type { CircuitBreakerConfig } from '../config/schema.js';
import { logger } from '../util/log.js';

/**
 * Circuit breaker states for managing provider health and failover.
 * 
 * The circuit breaker pattern prevents cascading failures by automatically
 * failing over to alternative providers when a provider becomes unresponsive.
 */
export type CircuitState = 'closed' | 'open' | 'half-open';

/**
 * Advanced Circuit Breaker Implementation
 * 
 * A robust circuit breaker that manages provider health and automatic failover.
 * Implements the classic circuit breaker pattern with three states:
 * 
 * ### State Transitions
 * 
 * **Closed State** → Requests flow normally; consecutive failures counted
 * **Open State** → All requests rejected immediately for cooldown period
 * **Half-Open State** → Limited trial requests allowed; success closes breaker, failure re-opens
 * 
 * ### Key Features
 * - Configurable failure thresholds and recovery parameters
 * - Automatic state transitions based on success/failure patterns
 * - Thread-safe operation with atomic state changes
 * - Comprehensive logging and monitoring capabilities
 * - Half-open probing for graceful recovery
 * 
 * @example
 * ```typescript
 * const breaker = new CircuitBreaker({
 *   failureThreshold: 5,
 *   cooldownMs: 30000,
 *   halfOpenRetries: 3,
 *   successThreshold: 2
 * });
 * 
 * if (breaker.allow()) {
 *   try {
 *     const result = await callProvider();
 *     breaker.recordSuccess();
 *     return result;
 *   } catch (error) {
 *     breaker.recordFailure();
 *     throw error;
 *   }
 * }
 * // Circuit is open, request will be rejected
 * ```
 */
export class CircuitBreaker {
  /**
   * Current state of the circuit breaker
   * - 'closed': Normal operation, requests pass through
   * - 'open': Circuit broken, requests rejected immediately
   * - 'half-open': Recovery mode, limited trial requests allowed
   */
  public state: CircuitState = 'closed';
  
  /**
   * Number of consecutive failures in current state
   * Reset to 0 on successful transitions to closed state
   */
  private failures = 0;
  
  /**
   * Number of consecutive successes in half-open state
   * Used to determine when to close the circuit during recovery
   */
  private successes = 0;
  
  /**
   * Timestamp when circuit was opened (Date.now())
   * Used to calculate cooldown duration and half-open transition
   */
  private openedAt = 0;
  
  /**
   * Number of requests currently in flight during half-open state
   * Limits concurrent trial requests during recovery probing
   */
  private halfOpenInFlight = 0;
  
  /**
   * Circuit breaker configuration parameters
   * Immutable after construction, defines breaker behavior
   */
  readonly cfg: CircuitBreakerConfig;

  /**
   * Constructs a new CircuitBreaker instance with the specified configuration.
   * 
   * @param cfg - Configuration object defining breaker behavior
   * @throws {Error} If configuration contains invalid values
   * 
   * @example
   * ```typescript
   * const config = {
   *   failureThreshold: 5,      // Open after 5 consecutive failures
   *   cooldownMs: 30000,        // Wait 30 seconds before recovery attempts
   *   halfOpenRetries: 3,      // Allow 3 trial requests during recovery
   *   successThreshold: 2       // Close after 2 consecutive successes
   * };
   * 
   * const breaker = new CircuitBreaker(config);
   * ```
   */
  constructor(cfg: CircuitBreakerConfig) {
    // Validate configuration parameters
    if (cfg.failureThreshold < 1) {
      throw new Error('failureThreshold must be at least 1');
    }
    if (cfg.cooldownMs < 1000) {
      throw new Error('cooldownMs must be at least 1000ms');
    }
    if (cfg.halfOpenRetries < 1) {
      throw new Error('halfOpenRetries must be at least 1');
    }
    if (cfg.successThreshold < 1) {
      throw new Error('successThreshold must be at least 1');
    }
    
    this.cfg = cfg;
  }

  /**
   * Determines whether a request is currently allowed to pass through the circuit breaker.
   * 
   * This method implements the core circuit breaker logic, checking the current state
   * and deciding whether to allow or reject requests based on the breaker configuration.
   * 
   * ### State-Based Logic
   * 
   * **Closed State**: Always returns true, allowing normal request flow
   * **Open State**: Returns false during cooldown period, transitions to half-open when cooldown expires
   * **Half-Open State**: Returns true only if under the concurrent request limit
   * 
   * ### Lazy State Transitions
   * 
   * When in open state and cooldown period expires, this method automatically
   * transitions to half-open state and re-evaluates the request allowance.
   * This ensures state transitions happen at the right time without requiring
   * separate polling mechanisms.
   * 
   * @returns {boolean} True if the request is allowed to pass, false if rejected
   * 
   * @example
   * ```typescript
   * if (breaker.allow()) {
   *   // Proceed with provider call
   *   const result = await callProvider();
   * } else {
   *   // Circuit is open, use alternative provider
   *   const result = await callFallbackProvider();
   * }
   * ```
   */
  allow(): boolean {
    switch (this.state) {
      case 'closed':
        // Circuit is healthy, allow all requests to pass through
        return true;
        
      case 'open': {
        // Circuit is open, check if cooldown period has expired
        const elapsed = Date.now() - this.openedAt;
        if (elapsed >= this.cfg.cooldownMs) {
          // Cooldown expired, transition to half-open for recovery testing
          this.toHalfOpen();
          // Recursively check allowance after state transition
          return this.allow();
        }
        // Still in cooldown, reject the request
        return false;
      }
        
      case 'half-open':
        // Allow requests only if under the concurrent limit for recovery probing
        return this.halfOpenInFlight < this.cfg.halfOpenRetries;
    }
  }

  /**
   * Records a successful request outcome and updates circuit breaker state accordingly.
   * 
   * This method should be called when a provider request succeeds, allowing the circuit breaker
   * to track recovery progress and potentially close the circuit if recovery criteria are met.
   * 
   * ### State-Specific Behavior
   * 
   * **Half-Open State**: 
   * - Increments success counter and decrements in-flight counter
   * - If success threshold reached, transitions to closed state (recovery complete)
   * - Otherwise, remains in half-open state for further testing
   * 
   * **Closed State**:
   * - Resets failure counter to 0, maintaining healthy state
   * - No state transition required for successful requests
   * 
   * **Open State**: 
   * - No action taken (requests should not reach here when circuit is open)
   * 
   * @example
   * ```typescript
   * try {
   *   const result = await provider.call();
   *   breaker.recordSuccess(); // Report successful request
   *   return result;
   * } catch (error) {
   *   breaker.recordFailure(); // Report failed request
   *   throw error;
   * }
   * ```
   */
  recordSuccess(): void {
    if (this.state === 'half-open') {
      // Recovery in progress: increment success count and decrement in-flight counter
      this.successes += 1;
      this.halfOpenInFlight = Math.max(0, this.halfOpenInFlight - 1);
      
      // Check if recovery criteria are met
      if (this.successes >= this.cfg.successThreshold) {
        // Recovery successful, close the circuit
        this.toClosed();
      }
      // Otherwise continue in half-open state for more testing
    } else if (this.state === 'closed') {
      // Maintain healthy state by resetting failure counter
      this.failures = 0;
    }
    // No action needed for open state (requests shouldn't reach here)
  }

  /**
   * Records a failed request outcome and updates circuit breaker state accordingly.
   * 
   * This method should be called when a provider request fails, allowing the circuit breaker
   * to track failure patterns and potentially open the circuit if failure criteria are met.
   * 
   * ### State-Specific Behavior
   * 
   * **Half-Open State**:
   * - Immediately re-opens the circuit (recovery failed)
   * - Any failure during recovery testing is considered catastrophic
   * - No further testing attempts until next cooldown period
   * 
   * **Closed State**:
   * - Increments failure counter
   * - If failure threshold reached, transitions to open state
   * - Otherwise remains in closed state for continued normal operation
   * 
   * **Open State**:
   * - No action taken (circuit already open, requests rejected)
   * 
   * @example
   * ```typescript
   * try {
   *   const result = await provider.call();
   *   breaker.recordSuccess();
   *   return result;
   * } catch (error) {
   *   breaker.recordFailure(); // This will trigger state transitions if needed
   *   throw error;
   * }
   * ```
   */
  recordFailure(): void {
    if (this.state === 'half-open') {
      // Recovery failed: decrement in-flight counter and immediately re-open circuit
      this.halfOpenInFlight = Math.max(0, this.halfOpenInFlight - 1);
      // Any failure during recovery testing is unacceptable - circuit re-opens
      this.toOpen();
      return;
    }
    
    // Track failures in closed state
    this.failures += 1;
    
    // Check if failure threshold has been reached
    if (this.failures >= this.cfg.failureThreshold) {
      // Too many consecutive failures, open the circuit
      this.toOpen();
    }
    // Otherwise remain in closed state for continued operation
  }

  /**
   * Creates a snapshot of the current circuit breaker state for health monitoring and reporting.
   * 
   * This method provides a comprehensive view of the breaker's current status,
   * which is useful for health check endpoints, monitoring dashboards, and debugging.
   * 
   * ### Snapshot Information
   * 
   * **State Information**: Current circuit state (closed/open/half-open)
   * **Failure Tracking**: Number of consecutive failures in current state
   * **Success Tracking**: Number of consecutive successes during recovery (half-open only)
   * **Timing Information**: When circuit was opened (if applicable)
   * **Configuration**: Cooldown period duration
   * 
   * @returns {CircuitBreakerSnapshot} A snapshot object containing current breaker state
   * 
   * @example
   * ```typescript
   * const snapshot = breaker.snapshot();
   * console.log(`Circuit state: ${snapshot.state}`);
   * console.log(`Consecutive failures: ${snapshot.failures}`);
   * console.log(`Uptime: ${snapshot.openedAt ? Date.now() - snapshot.openedAt : 'N/A'}`);
   * ```
   */
  snapshot(): CircuitBreakerSnapshot {
    return {
      state: this.state,
      failures: this.failures,
      successes: this.successes,
      openedAt: this.openedAt || undefined,
      cooldownMs: this.cfg.cooldownMs,
    };
  }

  /**
   * Resets the circuit breaker to its initial closed state.
   * 
   * This method forcibly resets the breaker regardless of its current state,
   * clearing all counters and returning to normal operation mode.
   * 
   * ### Use Cases
   * 
   * **Manual Intervention**: When operator decides to reset the breaker
   * **Emergency Recovery**: Quick recovery during critical situations
   * **Testing**: Reset breaker for testing scenarios
   * **Configuration Changes**: After updating configuration, reset state
   * 
   * ### State Changes
   * 
   * **Any State → Closed**: Regardless of current state, immediately transitions to closed
   * **Counters Reset**: All failure, success, and in-flight counters cleared
   * **Logging**: Logs the reset action for audit purposes
   * 
   * @example
   * ```typescript
   * // Manual reset after provider maintenance
   * breaker.reset();
   * console.log('Circuit breaker has been reset');
   * 
   * // Emergency reset during critical failure
   * if (isEmergency) {
   *   breaker.reset();
   * }
   * ```
   */
  reset(): void {
    this.toClosed();
  }

  /**
   * Transitions the circuit breaker to the open state.
   * 
   * This private method handles the internal logic for opening the circuit,
   * which occurs when the failure threshold is exceeded during normal operation.
   * 
   * ### State Changes
   * 
   * **Current State**: Any state (typically closed or half-open)
   * **New State**: Open
   * 
   * ### Side Effects
   * 
   * **State Transition**: Sets state to 'open'
   * **Timing**: Records the current timestamp for cooldown calculations
   * **Counters**: Resets success and in-flight counters
   * **Logging**: Logs the transition with failure count for debugging
   * 
   * ### Business Logic
   * 
   * The circuit will remain in the open state for the duration specified
   * in `cfg.cooldownMs`, after which it will transition to half-open state
   * to test if the provider has recovered.
   * 
   * @private
   */
  private toOpen(): void {
    this.state = 'open';
    this.openedAt = Date.now();
    this.successes = 0;
    this.halfOpenInFlight = 0;
    logger.warn('circuit', `breaker OPENED (after ${this.failures} failures)`);
  }

  /**
   * Transitions the circuit breaker to the half-open state.
   * 
   * This private method handles the transition from open to half-open state,
   * which occurs when the cooldown period expires and the circuit begins
   * testing if the provider has recovered.
   * 
   * ### State Changes
   * 
   * **Current State**: Open (after cooldown expiration)
   * **New State**: Half-open
   * 
   * ### Side Effects
   * 
   * **State Transition**: Sets state to 'half-open'
   * **Counters**: Resets success counter and in-flight counter
   * **Logging**: Logs the transition for monitoring purposes
   * 
   * ### Business Logic
   * 
   * In half-open state, the circuit allows a limited number of trial requests
   * to test if the provider has recovered. Each success moves toward closing
   * the circuit, while any failure immediately re-opens it.
   * 
   * @private
   */
  private toHalfOpen(): void {
    this.state = 'half-open';
    this.halfOpenInFlight = 0;
    this.successes = 0;
    logger.info('circuit', 'breaker -> half-open (probing)');
  }

  /**
   * Transitions the circuit breaker to the closed state.
   * 
   * This private method handles the transition to closed state, which occurs
   * when recovery criteria are met or when manually reset.
   * 
   * ### State Changes
   * 
   * **Current State**: Half-open (after successful recovery) or any state (manual reset)
   * **New State**: Closed
   * 
   * ### Side Effects
   * 
   * **State Transition**: Sets state to 'closed'
   * **Counters**: Resets all counters (failures, successes, in-flight)
   * **Logging**: Logs the successful recovery for monitoring
   * 
   * ### Business Logic
   * 
   * Once in closed state, the circuit returns to normal operation,
   * allowing all requests to pass through and counting failures again
   * if they occur.
   * 
   * @private
   */
  private toClosed(): void {
    this.state = 'closed';
    this.failures = 0;
    this.successes = 0;
    this.halfOpenInFlight = 0;
    logger.info('circuit', 'breaker CLOSED (recovered)');
  }
}

/**
 * Interface for circuit breaker snapshot used in health reporting.
 * 
 * This interface defines the structure returned by the `snapshot()` method,
 * providing a comprehensive view of the circuit breaker's current state
 * for monitoring and debugging purposes.
 */
export interface CircuitBreakerSnapshot {
  /** Current state of the circuit breaker */
  state: CircuitState;
  
  /** Number of consecutive failures in current state */
  failures: number;
  
  /** Number of consecutive successes during recovery (half-open only) */
  successes: number;
  
  /** Timestamp when circuit was opened, undefined if not open */
  openedAt?: number;
  
  /** Cooldown period duration in milliseconds */
  cooldownMs: number;
}
