#import "CoreSimulator.h"
#import "SimulatorKit.h"

#import <CoreMedia/CoreMedia.h>
#import <CoreImage/CoreImage.h>
#import <CoreVideo/CoreVideo.h>
#import <ImageIO/ImageIO.h>
#import <IOSurface/IOSurface.h>
#import <VideoToolbox/VideoToolbox.h>
#import <dlfcn.h>
#import <mach/mach_time.h>
#import <objc/runtime.h>

typedef void (*SimxFrameCallback)(const unsigned char *bytes,
                                  unsigned long length,
                                  long long encode_latency_ms,
                                  void *context);
typedef void (*SimxEncodedFrameCallback)(const unsigned char *bytes,
                                         unsigned long length,
                                         int keyframe,
                                         long long pts_ms,
                                         const unsigned char *config_bytes,
                                         unsigned long config_length,
                                         long long encode_latency_ms,
                                         void *context);
typedef IndigoMessage *(*SimxMouseMessageFn)(CGPoint *location, CGPoint *windowLocation, uint32_t target, NSInteger eventType, CGSize displaySize, uint32_t edge);
typedef IndigoMessage *(*SimxKeyboardMessageFn)(int keyCode, int operation);
typedef IndigoMessage *(*SimxButtonMessageFn)(uint32_t buttonCode, uint32_t operation, uint32_t target);
typedef IndigoMessage *(*SimxArbitraryHIDMessageFn)(uint32_t target, uint32_t page, uint32_t usage, uint32_t operation);

static uint32_t const SimxTouchTarget = 0x32;
static int const SimxKeyboardDown = 1;
static int const SimxKeyboardUp = 2;
static uint32_t const SimxButtonDown = 1;
static uint32_t const SimxButtonUp = 2;
static uint32_t const SimxButtonTargetHardware = 0x2;
static uint32_t const SimxConsumerControlUsagePage = 0x0c;
static uint32_t const SimxHomeMenuUsage = 0x40;
static uint32_t const SimxHomeUsage = 0x65;
static uint32_t const SimxHomeButtonCode = 0x191;
static size_t const SimxH264MaxEncodedWidth = 640;

typedef struct {
    BOOL useButtonMessage;
    uint32_t buttonCode;
    uint32_t target;
    uint32_t page;
    uint32_t usage;
} SimxHomeStrategy;

static void simx_set_error(char **error, NSString *message);
static uint64_t simx_elapsed_ns(uint64_t startedAt, uint64_t finishedAt);
static long long simx_elapsed_ms(uint64_t startedAt, uint64_t finishedAt);
static void simx_h264_output_callback(void *outputCallbackRefCon,
                                      void *sourceFrameRefCon,
                                      OSStatus status,
                                      VTEncodeInfoFlags infoFlags,
                                      CMSampleBufferRef sampleBuffer);

@interface SimxFrameStreamer : NSObject
@property (nonatomic, strong) SimDevice *device;
@property (nonatomic, strong) id<SimDisplayIOSurfaceRenderable> surface;
@property (nonatomic, strong) id<SimScreen> screen;
@property (nonatomic, strong) NSUUID *uuid;
@property (nonatomic, strong) CIContext *ciContext;
@property (nonatomic, strong) dispatch_queue_t encodeQueue;
@property (nonatomic, assign) uint32_t lastSeed;
@property (nonatomic, assign) float quality;
@property (nonatomic, assign) SimxFrameCallback callback;
@property (nonatomic, assign) void *callbackContext;
@property (nonatomic, assign) SimxEncodedFrameCallback encodedCallback;
@property (nonatomic, assign) void *encodedCallbackContext;
@property (nonatomic, assign) int targetFPS;
@property (nonatomic, assign) int bitrate;
@property (nonatomic, assign) VTCompressionSessionRef compressionSession;
@property (nonatomic, assign) int64_t videoFrameIndex;
@property (nonatomic, assign) uint64_t lastH264EncodeAt;
@property (nonatomic, assign) size_t encodedWidth;
@property (nonatomic, assign) size_t encodedHeight;
@property (atomic, assign) BOOL forceKeyframe;
@property (nonatomic, assign) BOOL stopped;
@property (nonatomic, strong) id hidClient;
@property (nonatomic, assign) SimxMouseMessageFn mouseMessage;
@property (nonatomic, assign) SimxKeyboardMessageFn keyboardMessage;
@property (nonatomic, assign) SimxButtonMessageFn buttonMessage;
@property (nonatomic, assign) SimxArbitraryHIDMessageFn arbitraryHIDMessage;
@property (nonatomic, assign) int hidTimeoutMs;
@end

@implementation SimxFrameStreamer

- (instancetype)initWithDevice:(SimDevice *)device
                       surface:(id<SimDisplayIOSurfaceRenderable>)surface
                        screen:(id<SimScreen>)screen
                          uuid:(NSUUID *)uuid
                       quality:(float)quality
                      callback:(SimxFrameCallback)callback
               callbackContext:(void *)callbackContext
{
    self = [super init];
    if (!self) { return nil; }
    _device = device;
    _surface = surface;
    _screen = screen;
    _uuid = uuid;
    _quality = quality;
    _callback = callback;
    _callbackContext = callbackContext;
    _targetFPS = 60;
    _bitrate = 8 * 1000 * 1000;
    _ciContext = [CIContext contextWithOptions:nil];
    _encodeQueue = dispatch_queue_create("simx.frame.encode", DISPATCH_QUEUE_SERIAL);
    return self;
}

- (void)handleSurface:(IOSurface *)surface
{
    if (self.stopped || surface == nil) { return; }
    IOSurface *retainedSurface = surface;
    dispatch_async(self.encodeQueue, ^{
        @try {
            if (self.stopped) {
                return;
            }
            IOSurfaceRef surfaceRef = (__bridge IOSurfaceRef)retainedSurface;
            IOSurfaceIncrementUseCount(surfaceRef);
            uint32_t seed = IOSurfaceGetSeed(surfaceRef);
            if (seed == self.lastSeed) {
                IOSurfaceDecrementUseCount(surfaceRef);
                return;
            }
            self.lastSeed = seed;
            if (self.encodedCallback != NULL) {
                uint64_t now = mach_absolute_time();
                BOOL forceKeyframe = self.forceKeyframe;
                if (!forceKeyframe && self.lastH264EncodeAt != 0 && self.targetFPS > 0) {
                    uint64_t maxInputFPS = ((uint64_t)MAX(1, self.targetFPS) * 3) / 2;
                    uint64_t minIntervalNs = 1000000000ULL / maxInputFPS;
                    if (simx_elapsed_ns(self.lastH264EncodeAt, now) < minIntervalNs) {
                        IOSurfaceDecrementUseCount(surfaceRef);
                        return;
                    }
                }
                self.lastH264EncodeAt = now;
                [self encodeH264Surface:surfaceRef];
                IOSurfaceDecrementUseCount(surfaceRef);
                return;
            }
            if (self.callback == NULL) {
                IOSurfaceDecrementUseCount(surfaceRef);
                return;
            }
            uint64_t encodeStartedAt = mach_absolute_time();
            CIImage *ciImage = [CIImage imageWithIOSurface:surfaceRef];
            if (ciImage == nil) {
                IOSurfaceDecrementUseCount(surfaceRef);
                return;
            }
            CGImageRef image = [self.ciContext createCGImage:ciImage fromRect:[ciImage extent]];
            IOSurfaceDecrementUseCount(surfaceRef);
            if (image == NULL) { return; }

            NSMutableData *data = [NSMutableData data];
            CGImageDestinationRef destination = CGImageDestinationCreateWithData((__bridge CFMutableDataRef)data, CFSTR("public.jpeg"), 1, NULL);
            if (destination == NULL) {
                CGImageRelease(image);
                return;
            }
            NSDictionary *options = @{(__bridge NSString *)kCGImageDestinationLossyCompressionQuality: @(self.quality)};
            CGImageDestinationAddImage(destination, image, (__bridge CFDictionaryRef)options);
            BOOL ok = CGImageDestinationFinalize(destination);
            CFRelease(destination);
            CGImageRelease(image);
            if (!ok || data.length == 0 || self.stopped) { return; }
            long long encodeLatencyMs = simx_elapsed_ms(encodeStartedAt, mach_absolute_time());
            self.callback((const unsigned char *)data.bytes,
                          (unsigned long)data.length,
                          encodeLatencyMs,
                          self.callbackContext);
        } @catch (NSException *exception) {
            NSLog(@"simx frame bridge exception: %@", exception);
        }
    });
}

- (BOOL)ensureCompressionSessionForSurface:(IOSurfaceRef)surfaceRef
{
    size_t width = IOSurfaceGetWidth(surfaceRef);
    size_t height = IOSurfaceGetHeight(surfaceRef);
    if (width == 0 || height == 0) { return NO; }
    if (width > SimxH264MaxEncodedWidth) {
        double scale = (double)SimxH264MaxEncodedWidth / (double)width;
        width = SimxH264MaxEncodedWidth;
        height = MAX((size_t)1, (size_t)llround((double)height * scale));
    }
    if (self.compressionSession != NULL &&
        self.encodedWidth == width &&
        self.encodedHeight == height) {
        return YES;
    }
    if (self.compressionSession != NULL) {
        VTCompressionSessionCompleteFrames(self.compressionSession, kCMTimeInvalid);
        VTCompressionSessionInvalidate(self.compressionSession);
        CFRelease(self.compressionSession);
        self.compressionSession = NULL;
        self.videoFrameIndex = 0;
        self.forceKeyframe = YES;
    }
    self.encodedWidth = width;
    self.encodedHeight = height;

    NSDictionary *encoderSpecification = @{
        (__bridge NSString *)kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: @YES,
        (__bridge NSString *)kVTVideoEncoderSpecification_EnableLowLatencyRateControl: @YES,
    };
    OSStatus status = VTCompressionSessionCreate(kCFAllocatorDefault,
                                                 (int32_t)width,
                                                 (int32_t)height,
                                                 kCMVideoCodecType_H264,
                                                 (__bridge CFDictionaryRef)encoderSpecification,
                                                 NULL,
                                                 NULL,
                                                 simx_h264_output_callback,
                                                 (__bridge void *)self,
                                                 &_compressionSession);
    if (status != noErr || self.compressionSession == NULL) {
        NSLog(@"simx h264 session creation failed: %d", status);
        return NO;
    }

    int fps = MAX(1, self.targetFPS);
    int bitrate = MAX(256 * 1000, self.bitrate);
    int keyframeInterval = fps * 2;
    int maxFrameDelay = 0;
    double keyframeIntervalDuration = 2.0;
    CFNumberRef fpsNumber = CFNumberCreate(kCFAllocatorDefault, kCFNumberIntType, &fps);
    CFNumberRef bitrateNumber = CFNumberCreate(kCFAllocatorDefault, kCFNumberIntType, &bitrate);
    CFNumberRef keyframeNumber = CFNumberCreate(kCFAllocatorDefault, kCFNumberIntType, &keyframeInterval);
    CFNumberRef maxFrameDelayNumber = CFNumberCreate(kCFAllocatorDefault, kCFNumberIntType, &maxFrameDelay);
    CFNumberRef keyframeDurationNumber = CFNumberCreate(kCFAllocatorDefault, kCFNumberDoubleType, &keyframeIntervalDuration);

    VTSessionSetProperty(self.compressionSession, kVTCompressionPropertyKey_RealTime, kCFBooleanTrue);
    VTSessionSetProperty(self.compressionSession, kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse);
    VTSessionSetProperty(self.compressionSession, kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, kCFBooleanTrue);
    VTSessionSetProperty(self.compressionSession, kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_Baseline_AutoLevel);
    VTSessionSetProperty(self.compressionSession, kVTCompressionPropertyKey_H264EntropyMode, kVTH264EntropyMode_CAVLC);
    if (maxFrameDelayNumber != NULL) {
        VTSessionSetProperty(self.compressionSession, kVTCompressionPropertyKey_MaxFrameDelayCount, maxFrameDelayNumber);
        CFRelease(maxFrameDelayNumber);
    }
    if (fpsNumber != NULL) {
        VTSessionSetProperty(self.compressionSession, kVTCompressionPropertyKey_ExpectedFrameRate, fpsNumber);
        CFRelease(fpsNumber);
    }
    if (bitrateNumber != NULL) {
        VTSessionSetProperty(self.compressionSession, kVTCompressionPropertyKey_AverageBitRate, bitrateNumber);
        CFRelease(bitrateNumber);
    }
    if (keyframeNumber != NULL) {
        VTSessionSetProperty(self.compressionSession, kVTCompressionPropertyKey_MaxKeyFrameInterval, keyframeNumber);
        CFRelease(keyframeNumber);
    }
    if (keyframeDurationNumber != NULL) {
        VTSessionSetProperty(self.compressionSession, kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, keyframeDurationNumber);
        CFRelease(keyframeDurationNumber);
    }
    VTCompressionSessionPrepareToEncodeFrames(self.compressionSession);
    return YES;
}

- (void)encodeH264Surface:(IOSurfaceRef)surfaceRef
{
    if (![self ensureCompressionSessionForSurface:surfaceRef]) { return; }
    uint64_t encodeStartedAtValue = mach_absolute_time();
    size_t sourceWidth = IOSurfaceGetWidth(surfaceRef);
    size_t sourceHeight = IOSurfaceGetHeight(surfaceRef);
    CVPixelBufferRef pixelBuffer = NULL;
    if (self.encodedWidth > 0 &&
        self.encodedHeight > 0 &&
        (self.encodedWidth != sourceWidth || self.encodedHeight != sourceHeight)) {
        NSDictionary *attributes = @{
            (__bridge NSString *)kCVPixelBufferPixelFormatTypeKey: @(kCVPixelFormatType_32BGRA),
            (__bridge NSString *)kCVPixelBufferWidthKey: @(self.encodedWidth),
            (__bridge NSString *)kCVPixelBufferHeightKey: @(self.encodedHeight),
            (__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey: @{},
        };
        CVReturn pixelStatus = CVPixelBufferCreate(kCFAllocatorDefault,
                                                   self.encodedWidth,
                                                   self.encodedHeight,
                                                   kCVPixelFormatType_32BGRA,
                                                   (__bridge CFDictionaryRef)attributes,
                                                   &pixelBuffer);
        if (pixelStatus != kCVReturnSuccess || pixelBuffer == NULL) {
            NSLog(@"simx h264 scaled pixel buffer creation failed: %d", pixelStatus);
            return;
        }
        CIImage *sourceImage = [CIImage imageWithIOSurface:surfaceRef];
        if (sourceImage == nil) {
            CFRelease(pixelBuffer);
            return;
        }
        CGFloat scaleX = (CGFloat)self.encodedWidth / (CGFloat)sourceWidth;
        CGFloat scaleY = (CGFloat)self.encodedHeight / (CGFloat)sourceHeight;
        CIImage *scaledImage = [sourceImage imageByApplyingTransform:CGAffineTransformMakeScale(scaleX, scaleY)];
        CGColorSpaceRef colorSpace = CGColorSpaceCreateDeviceRGB();
        [self.ciContext render:scaledImage
               toCVPixelBuffer:pixelBuffer
                         bounds:CGRectMake(0, 0, self.encodedWidth, self.encodedHeight)
                    colorSpace:colorSpace];
        if (colorSpace != NULL) { CGColorSpaceRelease(colorSpace); }
    } else {
        CVReturn pixelStatus = CVPixelBufferCreateWithIOSurface(kCFAllocatorDefault, surfaceRef, NULL, &pixelBuffer);
        if (pixelStatus != kCVReturnSuccess || pixelBuffer == NULL) {
            NSLog(@"simx h264 pixel buffer creation failed: %d", pixelStatus);
            return;
        }
    }

    CMTime pts = CMTimeMake(self.videoFrameIndex, MAX(1, self.targetFPS));
    self.videoFrameIndex += 1;
    uint64_t *encodeStartedAt = malloc(sizeof(uint64_t));
    if (encodeStartedAt != NULL) {
        *encodeStartedAt = encodeStartedAtValue;
    }
    NSDictionary *frameProperties = nil;
    if (self.forceKeyframe) {
        self.forceKeyframe = NO;
        frameProperties = @{(__bridge NSString *)kVTEncodeFrameOptionKey_ForceKeyFrame: @YES};
    }
    OSStatus status = VTCompressionSessionEncodeFrame(self.compressionSession,
                                                      pixelBuffer,
                                                      pts,
                                                      kCMTimeInvalid,
                                                      (__bridge CFDictionaryRef)frameProperties,
                                                      encodeStartedAt,
                                                      NULL);
    CFRelease(pixelBuffer);
    if (status != noErr) {
        if (encodeStartedAt != NULL) { free(encodeStartedAt); }
        NSLog(@"simx h264 encode failed: %d", status);
    }
}

- (void)stop
{
    if (self.stopped) { return; }
    self.stopped = YES;
    id surfaceObject = self.surface;
    id screenObject = self.screen;
    if (screenObject != nil && [screenObject respondsToSelector:@selector(unregisterScreenCallbacksWithUUID:)]) {
        [screenObject unregisterScreenCallbacksWithUUID:self.uuid];
    }
    if ([surfaceObject respondsToSelector:@selector(unregisterIOSurfaceChangeCallbackWithUUID:)]) {
        [surfaceObject unregisterIOSurfaceChangeCallbackWithUUID:self.uuid];
    }
    if ([surfaceObject respondsToSelector:@selector(unregisterIOSurfacesChangeCallbackWithUUID:)]) {
        [surfaceObject unregisterIOSurfacesChangeCallbackWithUUID:self.uuid];
    }
    if ([surfaceObject respondsToSelector:@selector(unregisterDamageRectanglesCallbackWithUUID:)]) {
        [surfaceObject unregisterDamageRectanglesCallbackWithUUID:self.uuid];
    }
    if (self.compressionSession != NULL) {
        VTCompressionSessionCompleteFrames(self.compressionSession, kCMTimeInvalid);
        VTCompressionSessionInvalidate(self.compressionSession);
        CFRelease(self.compressionSession);
        self.compressionSession = NULL;
    }
}

- (void)dealloc
{
    [self stop];
}

@end

@implementation SimxFrameStreamer (HID)

- (BOOL)sendMessage:(void *)message error:(char **)error
{
    if (message == NULL || self.hidClient == nil) {
        simx_set_error(error, @"SimulatorKit HID transport is unavailable.");
        return NO;
    }
    dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
    __block NSError *sendError = nil;
    @try {
        [self.hidClient sendWithMessage:message
                           freeWhenDone:YES
                        completionQueue:dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0)
                             completion:^(NSError *completionError) {
            sendError = completionError;
            dispatch_semaphore_signal(semaphore);
        }];
    } @catch (NSException *exception) {
        simx_set_error(error, [NSString stringWithFormat:@"SimulatorKit HID exception: %@", exception.reason ?: exception.name]);
        free(message);
        return NO;
    }
    int timeoutMs = self.hidTimeoutMs > 0 ? self.hidTimeoutMs : 2000;
    dispatch_time_t deadline = dispatch_time(DISPATCH_TIME_NOW, (int64_t)timeoutMs * NSEC_PER_MSEC);
    if (dispatch_semaphore_wait(semaphore, deadline) != 0) {
        simx_set_error(error, @"Timed out waiting for SimulatorKit HID delivery.");
        return NO;
    }
    if (sendError != nil) {
        simx_set_error(error, sendError.localizedDescription);
        return NO;
    }
    return YES;
}

- (BOOL)sendTouchX:(double)nx y:(double)ny down:(BOOL)down error:(char **)error
{
    if (self.mouseMessage == NULL) {
        simx_set_error(error, @"SimulatorKit did not expose IndigoHIDMessageForMouseNSEvent.");
        return NO;
    }
    nx = fmax(0.0, fmin(1.0, nx));
    ny = fmax(0.0, fmin(1.0, ny));
    CGPoint point = CGPointMake(nx, ny);
    CGSize displaySize = self.device.deviceType.mainScreenSize;
    NSInteger mouseEventType = down ? 1 : 2;
    IndigoMessage *baseMessage = self.mouseMessage(&point, NULL, SimxTouchTarget, mouseEventType, displaySize, 0);
    if (baseMessage == NULL) {
        simx_set_error(error, @"SimulatorKit failed to create the base touch HID packet.");
        return NO;
    }
    size_t messageSize = sizeof(IndigoMessage) + sizeof(IndigoPayload);
    IndigoMessage *message = calloc(1, messageSize);
    if (message == NULL) {
        free(baseMessage);
        simx_set_error(error, @"Unable to allocate touch HID packet.");
        return NO;
    }
    message->innerSize = (uint32_t)sizeof(IndigoPayload);
    message->eventType = 0x02;
    message->payload.field1 = 0x0000000b;
    message->payload.timestamp = mach_absolute_time();
    message->payload.event.touch = baseMessage->payload.event.touch;
    message->payload.event.touch.xRatio = nx;
    message->payload.event.touch.yRatio = ny;
    IndigoPayload *second = (IndigoPayload *)(((uint8_t *)&message->payload) + sizeof(IndigoPayload));
    memcpy(second, &message->payload, sizeof(IndigoPayload));
    second->event.touch.field1 = 0x00000001;
    second->event.touch.field2 = 0x00000002;
    free(baseMessage);
    return [self sendMessage:message error:error];
}

- (BOOL)sendKeyCode:(uint16_t)keyCode down:(BOOL)down error:(char **)error
{
    if (self.keyboardMessage == NULL) {
        simx_set_error(error, @"SimulatorKit did not expose IndigoHIDMessageForKeyboardArbitrary.");
        return NO;
    }
    IndigoMessage *message = self.keyboardMessage((int)keyCode, down ? SimxKeyboardDown : SimxKeyboardUp);
    if (message == NULL) {
        simx_set_error(error, @"SimulatorKit failed to create keyboard HID packet.");
        return NO;
    }
    return [self sendMessage:message error:error];
}

- (BOOL)pressHome:(char **)error
{
    static const SimxHomeStrategy strategies[] = {
        { NO, 0, SimxTouchTarget, SimxConsumerControlUsagePage, SimxHomeMenuUsage },
        { NO, 0, SimxTouchTarget, SimxConsumerControlUsagePage, SimxHomeUsage },
        { YES, SimxHomeButtonCode, SimxButtonTargetHardware, 0, 0 },
        { YES, SimxHomeButtonCode, SimxTouchTarget, 0, 0 },
    };
    NSString *lastError = nil;
    for (size_t index = 0; index < sizeof(strategies) / sizeof(strategies[0]); index++) {
        const SimxHomeStrategy *strategy = &strategies[index];
        IndigoMessage *down = NULL;
        IndigoMessage *up = NULL;
        if (strategy->useButtonMessage) {
            if (self.buttonMessage == NULL) {
                lastError = @"SimulatorKit did not expose IndigoHIDMessageForButton.";
                continue;
            }
            down = self.buttonMessage(strategy->buttonCode, SimxButtonDown, strategy->target);
            up = self.buttonMessage(strategy->buttonCode, SimxButtonUp, strategy->target);
        } else {
            if (self.arbitraryHIDMessage == NULL) {
                lastError = @"SimulatorKit did not expose IndigoHIDMessageForHIDArbitrary.";
                continue;
            }
            down = self.arbitraryHIDMessage(strategy->target, strategy->page, strategy->usage, SimxButtonDown);
            up = self.arbitraryHIDMessage(strategy->target, strategy->page, strategy->usage, SimxButtonUp);
        }
        if (down == NULL || up == NULL) {
            if (down != NULL) { free(down); }
            if (up != NULL) { free(up); }
            lastError = @"SimulatorKit could not create Home HID packets.";
            continue;
        }
        char *downError = NULL;
        BOOL downOK = [self sendMessage:down error:&downError];
        if (!downOK) {
            lastError = downError != NULL ? [NSString stringWithUTF8String:downError] : @"Home button down failed.";
            if (downError != NULL) { free(downError); }
            free(up);
            continue;
        }
        [NSThread sleepForTimeInterval:0.08];
        char *upError = NULL;
        BOOL upOK = [self sendMessage:up error:&upError];
        if (upOK) {
            return YES;
        }
        lastError = upError != NULL ? [NSString stringWithUTF8String:upError] : @"Home button up failed.";
        if (upError != NULL) { free(upError); }
    }
    simx_set_error(error, lastError ?: @"SimulatorKit rejected every Home HID strategy.");
    return NO;
}

@end

static char *simx_strdup(NSString *message) {
    if (message == nil) { message = @"Unknown native SimStream bridge error."; }
    const char *utf8 = message.UTF8String;
    if (utf8 == NULL) { utf8 = "Unknown native SimStream bridge error."; }
    return strdup(utf8);
}

static void simx_set_error(char **error, NSString *message) {
    if (error != NULL) { *error = simx_strdup(message); }
}

static uint64_t simx_elapsed_ns(uint64_t startedAt, uint64_t finishedAt) {
    static mach_timebase_info_data_t timebase;
    if (timebase.denom == 0) {
        mach_timebase_info(&timebase);
    }
    uint64_t elapsed = finishedAt >= startedAt ? finishedAt - startedAt : 0;
    return elapsed * timebase.numer / timebase.denom;
}

static long long simx_elapsed_ms(uint64_t startedAt, uint64_t finishedAt) {
    return (long long)(simx_elapsed_ns(startedAt, finishedAt) / 1000000);
}

void simx_bridge_free_string(char *value) {
    if (value != NULL) { free(value); }
}

static void simx_h264_output_callback(void *outputCallbackRefCon,
                                      void *sourceFrameRefCon,
                                      OSStatus status,
                                      VTEncodeInfoFlags infoFlags,
                                      CMSampleBufferRef sampleBuffer)
{
    (void)infoFlags;
    uint64_t encodeStartedAt = 0;
    if (sourceFrameRefCon != NULL) {
        encodeStartedAt = *((uint64_t *)sourceFrameRefCon);
        free(sourceFrameRefCon);
    }
    long long encodeLatencyMs = encodeStartedAt != 0 ? simx_elapsed_ms(encodeStartedAt, mach_absolute_time()) : -1;
    if (status != noErr || sampleBuffer == NULL || !CMSampleBufferDataIsReady(sampleBuffer)) {
        return;
    }

    SimxFrameStreamer *streamer = (__bridge SimxFrameStreamer *)outputCallbackRefCon;
    if (streamer == nil || streamer.stopped || streamer.encodedCallback == NULL) {
        return;
    }

    CMBlockBufferRef blockBuffer = CMSampleBufferGetDataBuffer(sampleBuffer);
    if (blockBuffer == NULL) { return; }

    size_t lengthAtOffset = 0;
    size_t totalLength = 0;
    char *dataPointer = NULL;
    OSStatus blockStatus = CMBlockBufferGetDataPointer(blockBuffer, 0, &lengthAtOffset, &totalLength, &dataPointer);
    if (blockStatus != noErr || dataPointer == NULL || totalLength == 0) {
        return;
    }

    BOOL keyframe = YES;
    CFArrayRef attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, false);
    if (attachments != NULL && CFArrayGetCount(attachments) > 0) {
        CFDictionaryRef attachment = CFArrayGetValueAtIndex(attachments, 0);
        keyframe = !CFDictionaryContainsKey(attachment, kCMSampleAttachmentKey_NotSync);
    }

    CMTime pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer);
    long long ptsMs = CMTIME_IS_VALID(pts) ? (long long)(CMTimeGetSeconds(pts) * 1000.0) : 0;
    NSMutableData *decoderConfig = nil;
    if (keyframe) {
        CMFormatDescriptionRef format = CMSampleBufferGetFormatDescription(sampleBuffer);
        const uint8_t *sps = NULL;
        const uint8_t *pps = NULL;
        size_t spsLength = 0;
        size_t ppsLength = 0;
        size_t parameterSetCount = 0;
        int nalUnitHeaderLength = 0;
        OSStatus spsStatus = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format,
                                                                                0,
                                                                                &sps,
                                                                                &spsLength,
                                                                                &parameterSetCount,
                                                                                &nalUnitHeaderLength);
        OSStatus ppsStatus = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format,
                                                                                1,
                                                                                &pps,
                                                                                &ppsLength,
                                                                                NULL,
                                                                                NULL);
        if (spsStatus == noErr && ppsStatus == noErr && sps != NULL && pps != NULL && spsLength >= 4 && ppsLength > 0) {
            decoderConfig = [NSMutableData data];
            uint8_t header[] = {
                1,
                sps[1],
                sps[2],
                sps[3],
                0xff,
                0xe1,
                (uint8_t)((spsLength >> 8) & 0xff),
                (uint8_t)(spsLength & 0xff),
            };
            [decoderConfig appendBytes:header length:sizeof(header)];
            [decoderConfig appendBytes:sps length:spsLength];
            uint8_t ppsHeader[] = {
                1,
                (uint8_t)((ppsLength >> 8) & 0xff),
                (uint8_t)(ppsLength & 0xff),
            };
            [decoderConfig appendBytes:ppsHeader length:sizeof(ppsHeader)];
            [decoderConfig appendBytes:pps length:ppsLength];
        }
    }
    streamer.encodedCallback((const unsigned char *)dataPointer,
                             (unsigned long)totalLength,
                             keyframe ? 1 : 0,
                             ptsMs,
                             decoderConfig.length > 0 ? (const unsigned char *)decoderConfig.bytes : NULL,
                             (unsigned long)decoderConfig.length,
                             encodeLatencyMs,
                             streamer.encodedCallbackContext);
}

void *simx_frame_stream_start(const char *developer_dir,
                              const char *udid,
                              float quality,
                              SimxFrameCallback callback,
                              void *callback_context,
                              int target_fps,
                              int bitrate,
                              SimxEncodedFrameCallback encoded_callback,
                              void *encoded_callback_context,
                              int hid_timeout_ms,
                              char **error)
{
    @autoreleasepool {
        if (callback == NULL && encoded_callback == NULL) {
            simx_set_error(error, @"Frame or encoded callback is required.");
            return NULL;
        }

        NSString *devDir = developer_dir != NULL ? [NSString stringWithUTF8String:developer_dir] : nil;
        NSString *targetUDID = udid != NULL ? [NSString stringWithUTF8String:udid] : nil;
        if (devDir.length == 0 || targetUDID.length == 0) {
            simx_set_error(error, @"Developer dir and simulator UDID are required.");
            return NULL;
        }

        NSString *coreSimulatorPath = @"/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator";
        void *coreHandle = dlopen(coreSimulatorPath.fileSystemRepresentation, RTLD_NOW | RTLD_GLOBAL);
        if (coreHandle == NULL) {
            simx_set_error(error, [NSString stringWithFormat:@"Could not load CoreSimulator: %s", dlerror()]);
            return NULL;
        }

        NSString *simulatorKitPath = [devDir stringByAppendingPathComponent:@"Library/PrivateFrameworks/SimulatorKit.framework/SimulatorKit"];
        void *simKitHandle = dlopen(simulatorKitPath.fileSystemRepresentation, RTLD_NOW | RTLD_GLOBAL);
        if (simKitHandle == NULL) {
            simx_set_error(error, [NSString stringWithFormat:@"Could not load SimulatorKit: %s", dlerror()]);
            return NULL;
        }

        Class contextClass = NSClassFromString(@"SimServiceContext");
        if (contextClass == Nil) {
            simx_set_error(error, @"CoreSimulator did not expose SimServiceContext.");
            return NULL;
        }
        NSError *contextError = nil;
        id context = [contextClass sharedServiceContextForDeveloperDir:devDir error:&contextError];
        if (context == nil) {
            simx_set_error(error, contextError.localizedDescription ?: @"CoreSimulator service context failed.");
            return NULL;
        }
        NSError *deviceSetError = nil;
        SimDeviceSet *deviceSet = [context defaultDeviceSetWithError:&deviceSetError];
        if (deviceSet == nil) {
            simx_set_error(error, deviceSetError.localizedDescription ?: @"Could not load default simulator device set.");
            return NULL;
        }

        SimDevice *target = nil;
        for (SimDevice *device in deviceSet.availableDevices) {
            if ([[device.UDID UUIDString] caseInsensitiveCompare:targetUDID] == NSOrderedSame) {
                target = device;
                break;
            }
        }
        if (target == nil) {
            simx_set_error(error, [NSString stringWithFormat:@"Simulator %@ was not found.", targetUDID]);
            return NULL;
        }
        if (target.state != 3) {
            simx_set_error(error, [NSString stringWithFormat:@"Simulator %@ is not booted.", targetUDID]);
            return NULL;
        }
        id hidClient = nil;
        Class clientClass = objc_lookUpClass("SimulatorKit.SimDeviceLegacyHIDClient");
        if (clientClass != Nil) {
            NSError *hidError = nil;
            @try {
                hidClient = [[clientClass alloc] initWithDevice:target error:&hidError];
            } @catch (NSException *exception) {
                NSLog(@"simx HID client init exception: %@", exception);
                hidClient = nil;
            }
            (void)hidError;
        }
        id<SimDeviceIOProtocol> io = target.io;
        if (io == nil) {
            simx_set_error(error, @"Booted simulator did not expose IO ports.");
            return NULL;
        }

        id<SimDisplayIOSurfaceRenderable> mainSurface = nil;
        id<SimScreen> mainScreen = nil;
        for (id port in io.ioPorts) {
            if (![port conformsToProtocol:@protocol(SimDeviceIOPortInterface)]) { continue; }
            id descriptor = [(id<SimDeviceIOPortInterface>)port descriptor];
            if (![descriptor conformsToProtocol:@protocol(SimDisplayRenderable)] ||
                ![descriptor conformsToProtocol:@protocol(SimDisplayIOSurfaceRenderable)]) {
                continue;
            }
            if ([descriptor respondsToSelector:@selector(state)]) {
                id state = [descriptor performSelector:@selector(state)];
                if ([state respondsToSelector:@selector(displayClass)] &&
                    [(id<SimDisplayDescriptorState>)state displayClass] != 0) {
                    continue;
                }
            }
            mainSurface = (id<SimDisplayIOSurfaceRenderable>)descriptor;
            if ([descriptor conformsToProtocol:@protocol(SimScreen)]) {
                mainScreen = (id<SimScreen>)descriptor;
            }
            break;
        }
        if (mainSurface == nil) {
            simx_set_error(error, @"Could not find main IOSurface display.");
            return NULL;
        }

        NSUUID *uuid = [NSUUID UUID];
        dispatch_queue_t callbackQueue = dispatch_queue_create("simx.frame.callbacks", DISPATCH_QUEUE_SERIAL);
        SimxFrameStreamer *streamer = [[SimxFrameStreamer alloc] initWithDevice:target
                                                                        surface:mainSurface
                                                                         screen:mainScreen
                                                                           uuid:uuid
                                                                        quality:fmaxf(0.0f, fminf(1.0f, quality))
                                                                       callback:callback
                                                                callbackContext:callback_context];
        streamer.hidClient = hidClient;
        streamer.targetFPS = target_fps > 0 ? target_fps : 60;
        streamer.bitrate = bitrate > 0 ? bitrate : 8 * 1000 * 1000;
        streamer.hidTimeoutMs = hid_timeout_ms > 0 ? hid_timeout_ms : 2000;
        streamer.encodedCallback = encoded_callback;
        streamer.encodedCallbackContext = encoded_callback_context;
        streamer.mouseMessage = (SimxMouseMessageFn)dlsym(simKitHandle, "IndigoHIDMessageForMouseNSEvent");
        streamer.keyboardMessage = (SimxKeyboardMessageFn)dlsym(simKitHandle, "IndigoHIDMessageForKeyboardArbitrary");
        streamer.buttonMessage = (SimxButtonMessageFn)dlsym(simKitHandle, "IndigoHIDMessageForButton");
        streamer.arbitraryHIDMessage = (SimxArbitraryHIDMessageFn)dlsym(simKitHandle, "IndigoHIDMessageForHIDArbitrary");

        __weak SimxFrameStreamer *weakStreamer = streamer;
        id surfaceObject = mainSurface;
        if (mainScreen != nil && [mainScreen respondsToSelector:@selector(registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:)]) {
            [mainScreen registerScreenCallbacksWithUUID:uuid
                                          callbackQueue:callbackQueue
                                          frameCallback:^{
                SimxFrameStreamer *strong = weakStreamer;
                IOSurface *surface = mainSurface.framebufferSurface ?: mainSurface.maskedFramebufferSurface ?: mainSurface.ioSurface;
                [strong handleSurface:surface];
            } surfacesChangedCallback:^(IOSurface *framebufferSurface, IOSurface *maskedFramebufferSurface) {
                SimxFrameStreamer *strong = weakStreamer;
                [strong handleSurface:(framebufferSurface ?: maskedFramebufferSurface)];
            } propertiesChangedCallback:^(id<SimScreenProperties> properties) {
                (void)properties;
            }];
        } else {
            if ([surfaceObject respondsToSelector:@selector(registerCallbackWithUUID:ioSurfaceChangeCallback:)]) {
                [mainSurface registerCallbackWithUUID:uuid ioSurfaceChangeCallback:^(IOSurface *surface) {
                    SimxFrameStreamer *strong = weakStreamer;
                    [strong handleSurface:surface];
                }];
            }
            if ([surfaceObject respondsToSelector:@selector(registerCallbackWithUUID:ioSurfacesChangeCallback:)]) {
                [mainSurface registerCallbackWithUUID:uuid ioSurfacesChangeCallback:^(IOSurface *framebufferSurface, IOSurface *maskedFramebufferSurface) {
                    SimxFrameStreamer *strong = weakStreamer;
                    [strong handleSurface:(framebufferSurface ?: maskedFramebufferSurface)];
                }];
            }
            if ([surfaceObject respondsToSelector:@selector(registerCallbackWithUUID:damageRectanglesCallback:)]) {
                [mainSurface registerCallbackWithUUID:uuid damageRectanglesCallback:^(NSArray<NSValue *> *rects) {
                    (void)rects;
                    SimxFrameStreamer *strong = weakStreamer;
                    IOSurface *surface = mainSurface.framebufferSurface ?: mainSurface.maskedFramebufferSurface ?: mainSurface.ioSurface;
                    [strong handleSurface:surface];
                }];
            }
        }

        IOSurface *initialSurface = mainSurface.framebufferSurface ?: mainSurface.maskedFramebufferSurface ?: mainSurface.ioSurface;
        [streamer handleSurface:initialSurface];

        return (__bridge_retained void *)streamer;
    }
}

void simx_frame_stream_stop(void *handle)
{
    if (handle == NULL) { return; }
    @autoreleasepool {
        SimxFrameStreamer *streamer = (__bridge_transfer SimxFrameStreamer *)handle;
        [streamer stop];
    }
}

int simx_stream_request_keyframe(void *handle, char **error)
{
    if (handle == NULL) {
        simx_set_error(error, @"Native stream handle was NULL.");
        return 0;
    }
    @try {
        SimxFrameStreamer *streamer = (__bridge SimxFrameStreamer *)handle;
        streamer.forceKeyframe = YES;
        return 1;
    } @catch (NSException *exception) {
        simx_set_error(error, [NSString stringWithFormat:@"SimulatorKit keyframe request exception: %@", exception.reason ?: exception.name]);
        return 0;
    }
}

int simx_hid_touch(void *handle, double nx, double ny, int down, char **error)
{
    if (handle == NULL) {
        simx_set_error(error, @"Native stream handle was NULL.");
        return 0;
    }
    @try {
        SimxFrameStreamer *streamer = (__bridge SimxFrameStreamer *)handle;
        return [streamer sendTouchX:nx y:ny down:(down != 0) error:error] ? 1 : 0;
    } @catch (NSException *exception) {
        simx_set_error(error, [NSString stringWithFormat:@"SimulatorKit touch exception: %@", exception.reason ?: exception.name]);
        return 0;
    }
}

int simx_hid_key(void *handle, unsigned short keyCode, int down, char **error)
{
    if (handle == NULL) {
        simx_set_error(error, @"Native stream handle was NULL.");
        return 0;
    }
    @try {
        SimxFrameStreamer *streamer = (__bridge SimxFrameStreamer *)handle;
        return [streamer sendKeyCode:keyCode down:(down != 0) error:error] ? 1 : 0;
    } @catch (NSException *exception) {
        simx_set_error(error, [NSString stringWithFormat:@"SimulatorKit key exception: %@", exception.reason ?: exception.name]);
        return 0;
    }
}

int simx_hid_home(void *handle, char **error)
{
    if (handle == NULL) {
        simx_set_error(error, @"Native stream handle was NULL.");
        return 0;
    }
    @try {
        SimxFrameStreamer *streamer = (__bridge SimxFrameStreamer *)handle;
        return [streamer pressHome:error] ? 1 : 0;
    } @catch (NSException *exception) {
        simx_set_error(error, [NSString stringWithFormat:@"SimulatorKit Home exception: %@", exception.reason ?: exception.name]);
        return 0;
    }
}
