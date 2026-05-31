#import <Foundation/Foundation.h>

@class SimDeviceSet;
@class SimDeviceType;
@class SimRuntime;
@protocol SimDeviceIOProtocol;

@interface SimServiceContext : NSObject
+ (nullable instancetype)sharedServiceContextForDeveloperDir:(NSString *)developerDir
                                                       error:(NSError **)error;
- (nullable SimDeviceSet *)defaultDeviceSetWithError:(NSError **)error;
@end

@interface SimDevice : NSObject
@property (nonatomic, readonly) NSUUID *UDID;
@property (nonatomic, readonly) NSString *name;
@property (nonatomic, readonly) unsigned int state;
@property (nonatomic, readonly, nullable) SimDeviceType *deviceType;
@property (nonatomic, readonly, nullable) SimRuntime *runtime;
@property (nonatomic, readonly, nullable) id<SimDeviceIOProtocol> io;
@end

@interface SimDeviceSet : NSObject
@property (nonatomic, readonly) NSArray<SimDevice *> *devices;
@property (nonatomic, readonly) NSArray<SimDevice *> *availableDevices;
@end

@interface SimDeviceType : NSObject
@property (nonatomic, readonly) CGSize mainScreenSize;
@property (nonatomic, readonly) float mainScreenScale;
@property (nonatomic, readonly) NSString *name;
@end

@interface SimRuntime : NSObject
@property (nonatomic, readonly) NSString *name;
@property (nonatomic, readonly) NSString *versionString;
@property (nonatomic, readonly) BOOL available;
@end
