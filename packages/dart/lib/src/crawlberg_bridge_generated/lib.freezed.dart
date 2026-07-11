// GENERATED CODE - DO NOT MODIFY BY HAND
// coverage:ignore-file
// ignore_for_file: type=lint
// ignore_for_file: unused_element, deprecated_member_use, deprecated_member_use_from_same_package, use_function_type_syntax_for_parameters, unnecessary_const, avoid_init_to_null, invalid_override_different_default_values_named, prefer_expression_function_bodies, annotate_overrides, invalid_annotation_target, unnecessary_question_mark

part of 'lib.dart';

// **************************************************************************
// FreezedGenerator
// **************************************************************************

// dart format off
T _$identity<T>(T value) => value;
/// @nodoc
mixin _$AuthConfig {





@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is AuthConfig);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
  return 'AuthConfig()';
}


}

/// @nodoc
class $AuthConfigCopyWith<$Res>  {
$AuthConfigCopyWith(AuthConfig _, $Res Function(AuthConfig) __);
}


/// Adds pattern-matching-related methods to [AuthConfig].
extension AuthConfigPatterns on AuthConfig {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( AuthConfig_Basic value)?  basic,TResult Function( AuthConfig_Bearer value)?  bearer,TResult Function( AuthConfig_Header value)?  header,required TResult orElse(),}){
final _that = this;
switch (_that) {
case AuthConfig_Basic() when basic != null:
return basic(_that);case AuthConfig_Bearer() when bearer != null:
return bearer(_that);case AuthConfig_Header() when header != null:
return header(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( AuthConfig_Basic value)  basic,required TResult Function( AuthConfig_Bearer value)  bearer,required TResult Function( AuthConfig_Header value)  header,}){
final _that = this;
switch (_that) {
case AuthConfig_Basic():
return basic(_that);case AuthConfig_Bearer():
return bearer(_that);case AuthConfig_Header():
return header(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( AuthConfig_Basic value)?  basic,TResult? Function( AuthConfig_Bearer value)?  bearer,TResult? Function( AuthConfig_Header value)?  header,}){
final _that = this;
switch (_that) {
case AuthConfig_Basic() when basic != null:
return basic(_that);case AuthConfig_Bearer() when bearer != null:
return bearer(_that);case AuthConfig_Header() when header != null:
return header(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function( String username,  String password)?  basic,TResult Function( String token)?  bearer,TResult Function( String name,  String value)?  header,required TResult orElse(),}) {final _that = this;
switch (_that) {
case AuthConfig_Basic() when basic != null:
return basic(_that.username,_that.password);case AuthConfig_Bearer() when bearer != null:
return bearer(_that.token);case AuthConfig_Header() when header != null:
return header(_that.name,_that.value);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function( String username,  String password)  basic,required TResult Function( String token)  bearer,required TResult Function( String name,  String value)  header,}) {final _that = this;
switch (_that) {
case AuthConfig_Basic():
return basic(_that.username,_that.password);case AuthConfig_Bearer():
return bearer(_that.token);case AuthConfig_Header():
return header(_that.name,_that.value);}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function( String username,  String password)?  basic,TResult? Function( String token)?  bearer,TResult? Function( String name,  String value)?  header,}) {final _that = this;
switch (_that) {
case AuthConfig_Basic() when basic != null:
return basic(_that.username,_that.password);case AuthConfig_Bearer() when bearer != null:
return bearer(_that.token);case AuthConfig_Header() when header != null:
return header(_that.name,_that.value);case _:
  return null;

}
}

}

/// @nodoc


class AuthConfig_Basic extends AuthConfig {
  const AuthConfig_Basic({required this.username, required this.password}): super._();
  

/// Username sent in the `Authorization: Basic` header.
 final  String username;
/// Password sent in the `Authorization: Basic` header.
 final  String password;

/// Create a copy of AuthConfig
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$AuthConfig_BasicCopyWith<AuthConfig_Basic> get copyWith => _$AuthConfig_BasicCopyWithImpl<AuthConfig_Basic>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is AuthConfig_Basic&&(identical(other.username, username) || other.username == username)&&(identical(other.password, password) || other.password == password));
}


@override
int get hashCode => Object.hash(runtimeType,username,password);

@override
String toString() {
  return 'AuthConfig.basic(username: $username, password: $password)';
}


}

/// @nodoc
abstract mixin class $AuthConfig_BasicCopyWith<$Res> implements $AuthConfigCopyWith<$Res> {
  factory $AuthConfig_BasicCopyWith(AuthConfig_Basic value, $Res Function(AuthConfig_Basic) _then) = _$AuthConfig_BasicCopyWithImpl;
@useResult
$Res call({
 String username, String password
});




}
/// @nodoc
class _$AuthConfig_BasicCopyWithImpl<$Res>
    implements $AuthConfig_BasicCopyWith<$Res> {
  _$AuthConfig_BasicCopyWithImpl(this._self, this._then);

  final AuthConfig_Basic _self;
  final $Res Function(AuthConfig_Basic) _then;

/// Create a copy of AuthConfig
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? username = null,Object? password = null,}) {
  return _then(AuthConfig_Basic(
username: null == username ? _self.username : username // ignore: cast_nullable_to_non_nullable
as String,password: null == password ? _self.password : password // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class AuthConfig_Bearer extends AuthConfig {
  const AuthConfig_Bearer({required this.token}): super._();
  

/// Token sent in the `Authorization: Bearer` header.
 final  String token;

/// Create a copy of AuthConfig
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$AuthConfig_BearerCopyWith<AuthConfig_Bearer> get copyWith => _$AuthConfig_BearerCopyWithImpl<AuthConfig_Bearer>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is AuthConfig_Bearer&&(identical(other.token, token) || other.token == token));
}


@override
int get hashCode => Object.hash(runtimeType,token);

@override
String toString() {
  return 'AuthConfig.bearer(token: $token)';
}


}

/// @nodoc
abstract mixin class $AuthConfig_BearerCopyWith<$Res> implements $AuthConfigCopyWith<$Res> {
  factory $AuthConfig_BearerCopyWith(AuthConfig_Bearer value, $Res Function(AuthConfig_Bearer) _then) = _$AuthConfig_BearerCopyWithImpl;
@useResult
$Res call({
 String token
});




}
/// @nodoc
class _$AuthConfig_BearerCopyWithImpl<$Res>
    implements $AuthConfig_BearerCopyWith<$Res> {
  _$AuthConfig_BearerCopyWithImpl(this._self, this._then);

  final AuthConfig_Bearer _self;
  final $Res Function(AuthConfig_Bearer) _then;

/// Create a copy of AuthConfig
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? token = null,}) {
  return _then(AuthConfig_Bearer(
token: null == token ? _self.token : token // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class AuthConfig_Header extends AuthConfig {
  const AuthConfig_Header({required this.name, required this.value}): super._();
  

/// HTTP header name to set on each request.
 final  String name;
/// HTTP header value to send.
 final  String value;

/// Create a copy of AuthConfig
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$AuthConfig_HeaderCopyWith<AuthConfig_Header> get copyWith => _$AuthConfig_HeaderCopyWithImpl<AuthConfig_Header>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is AuthConfig_Header&&(identical(other.name, name) || other.name == name)&&(identical(other.value, value) || other.value == value));
}


@override
int get hashCode => Object.hash(runtimeType,name,value);

@override
String toString() {
  return 'AuthConfig.header(name: $name, value: $value)';
}


}

/// @nodoc
abstract mixin class $AuthConfig_HeaderCopyWith<$Res> implements $AuthConfigCopyWith<$Res> {
  factory $AuthConfig_HeaderCopyWith(AuthConfig_Header value, $Res Function(AuthConfig_Header) _then) = _$AuthConfig_HeaderCopyWithImpl;
@useResult
$Res call({
 String name, String value
});




}
/// @nodoc
class _$AuthConfig_HeaderCopyWithImpl<$Res>
    implements $AuthConfig_HeaderCopyWith<$Res> {
  _$AuthConfig_HeaderCopyWithImpl(this._self, this._then);

  final AuthConfig_Header _self;
  final $Res Function(AuthConfig_Header) _then;

/// Create a copy of AuthConfig
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? name = null,Object? value = null,}) {
  return _then(AuthConfig_Header(
name: null == name ? _self.name : name // ignore: cast_nullable_to_non_nullable
as String,value: null == value ? _self.value : value // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc
mixin _$CrawlError {





@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
  return 'CrawlError()';
}


}

/// @nodoc
class $CrawlErrorCopyWith<$Res>  {
$CrawlErrorCopyWith(CrawlError _, $Res Function(CrawlError) __);
}


/// Adds pattern-matching-related methods to [CrawlError].
extension CrawlErrorPatterns on CrawlError {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( CrawlError_NotFound value)?  notFound,TResult Function( CrawlError_Unauthorized value)?  unauthorized,TResult Function( CrawlError_Forbidden value)?  forbidden,TResult Function( CrawlError_WafBlocked value)?  wafBlocked,TResult Function( CrawlError_Timeout value)?  timeout,TResult Function( CrawlError_RateLimited value)?  rateLimited,TResult Function( CrawlError_ServerError value)?  serverError,TResult Function( CrawlError_BadGateway value)?  badGateway,TResult Function( CrawlError_Gone value)?  gone,TResult Function( CrawlError_Connection value)?  connection,TResult Function( CrawlError_Dns value)?  dns,TResult Function( CrawlError_Ssl value)?  ssl,TResult Function( CrawlError_DataLoss value)?  dataLoss,TResult Function( CrawlError_BrowserError value)?  browserError,TResult Function( CrawlError_BrowserTimeout value)?  browserTimeout,TResult Function( CrawlError_InvalidConfig value)?  invalidConfig,TResult Function( CrawlError_Unsupported value)?  unsupported,TResult Function( CrawlError_SsrfPolicyViolation value)?  ssrfPolicyViolation,TResult Function( CrawlError_Other value)?  other,required TResult orElse(),}){
final _that = this;
switch (_that) {
case CrawlError_NotFound() when notFound != null:
return notFound(_that);case CrawlError_Unauthorized() when unauthorized != null:
return unauthorized(_that);case CrawlError_Forbidden() when forbidden != null:
return forbidden(_that);case CrawlError_WafBlocked() when wafBlocked != null:
return wafBlocked(_that);case CrawlError_Timeout() when timeout != null:
return timeout(_that);case CrawlError_RateLimited() when rateLimited != null:
return rateLimited(_that);case CrawlError_ServerError() when serverError != null:
return serverError(_that);case CrawlError_BadGateway() when badGateway != null:
return badGateway(_that);case CrawlError_Gone() when gone != null:
return gone(_that);case CrawlError_Connection() when connection != null:
return connection(_that);case CrawlError_Dns() when dns != null:
return dns(_that);case CrawlError_Ssl() when ssl != null:
return ssl(_that);case CrawlError_DataLoss() when dataLoss != null:
return dataLoss(_that);case CrawlError_BrowserError() when browserError != null:
return browserError(_that);case CrawlError_BrowserTimeout() when browserTimeout != null:
return browserTimeout(_that);case CrawlError_InvalidConfig() when invalidConfig != null:
return invalidConfig(_that);case CrawlError_Unsupported() when unsupported != null:
return unsupported(_that);case CrawlError_SsrfPolicyViolation() when ssrfPolicyViolation != null:
return ssrfPolicyViolation(_that);case CrawlError_Other() when other != null:
return other(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( CrawlError_NotFound value)  notFound,required TResult Function( CrawlError_Unauthorized value)  unauthorized,required TResult Function( CrawlError_Forbidden value)  forbidden,required TResult Function( CrawlError_WafBlocked value)  wafBlocked,required TResult Function( CrawlError_Timeout value)  timeout,required TResult Function( CrawlError_RateLimited value)  rateLimited,required TResult Function( CrawlError_ServerError value)  serverError,required TResult Function( CrawlError_BadGateway value)  badGateway,required TResult Function( CrawlError_Gone value)  gone,required TResult Function( CrawlError_Connection value)  connection,required TResult Function( CrawlError_Dns value)  dns,required TResult Function( CrawlError_Ssl value)  ssl,required TResult Function( CrawlError_DataLoss value)  dataLoss,required TResult Function( CrawlError_BrowserError value)  browserError,required TResult Function( CrawlError_BrowserTimeout value)  browserTimeout,required TResult Function( CrawlError_InvalidConfig value)  invalidConfig,required TResult Function( CrawlError_Unsupported value)  unsupported,required TResult Function( CrawlError_SsrfPolicyViolation value)  ssrfPolicyViolation,required TResult Function( CrawlError_Other value)  other,}){
final _that = this;
switch (_that) {
case CrawlError_NotFound():
return notFound(_that);case CrawlError_Unauthorized():
return unauthorized(_that);case CrawlError_Forbidden():
return forbidden(_that);case CrawlError_WafBlocked():
return wafBlocked(_that);case CrawlError_Timeout():
return timeout(_that);case CrawlError_RateLimited():
return rateLimited(_that);case CrawlError_ServerError():
return serverError(_that);case CrawlError_BadGateway():
return badGateway(_that);case CrawlError_Gone():
return gone(_that);case CrawlError_Connection():
return connection(_that);case CrawlError_Dns():
return dns(_that);case CrawlError_Ssl():
return ssl(_that);case CrawlError_DataLoss():
return dataLoss(_that);case CrawlError_BrowserError():
return browserError(_that);case CrawlError_BrowserTimeout():
return browserTimeout(_that);case CrawlError_InvalidConfig():
return invalidConfig(_that);case CrawlError_Unsupported():
return unsupported(_that);case CrawlError_SsrfPolicyViolation():
return ssrfPolicyViolation(_that);case CrawlError_Other():
return other(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( CrawlError_NotFound value)?  notFound,TResult? Function( CrawlError_Unauthorized value)?  unauthorized,TResult? Function( CrawlError_Forbidden value)?  forbidden,TResult? Function( CrawlError_WafBlocked value)?  wafBlocked,TResult? Function( CrawlError_Timeout value)?  timeout,TResult? Function( CrawlError_RateLimited value)?  rateLimited,TResult? Function( CrawlError_ServerError value)?  serverError,TResult? Function( CrawlError_BadGateway value)?  badGateway,TResult? Function( CrawlError_Gone value)?  gone,TResult? Function( CrawlError_Connection value)?  connection,TResult? Function( CrawlError_Dns value)?  dns,TResult? Function( CrawlError_Ssl value)?  ssl,TResult? Function( CrawlError_DataLoss value)?  dataLoss,TResult? Function( CrawlError_BrowserError value)?  browserError,TResult? Function( CrawlError_BrowserTimeout value)?  browserTimeout,TResult? Function( CrawlError_InvalidConfig value)?  invalidConfig,TResult? Function( CrawlError_Unsupported value)?  unsupported,TResult? Function( CrawlError_SsrfPolicyViolation value)?  ssrfPolicyViolation,TResult? Function( CrawlError_Other value)?  other,}){
final _that = this;
switch (_that) {
case CrawlError_NotFound() when notFound != null:
return notFound(_that);case CrawlError_Unauthorized() when unauthorized != null:
return unauthorized(_that);case CrawlError_Forbidden() when forbidden != null:
return forbidden(_that);case CrawlError_WafBlocked() when wafBlocked != null:
return wafBlocked(_that);case CrawlError_Timeout() when timeout != null:
return timeout(_that);case CrawlError_RateLimited() when rateLimited != null:
return rateLimited(_that);case CrawlError_ServerError() when serverError != null:
return serverError(_that);case CrawlError_BadGateway() when badGateway != null:
return badGateway(_that);case CrawlError_Gone() when gone != null:
return gone(_that);case CrawlError_Connection() when connection != null:
return connection(_that);case CrawlError_Dns() when dns != null:
return dns(_that);case CrawlError_Ssl() when ssl != null:
return ssl(_that);case CrawlError_DataLoss() when dataLoss != null:
return dataLoss(_that);case CrawlError_BrowserError() when browserError != null:
return browserError(_that);case CrawlError_BrowserTimeout() when browserTimeout != null:
return browserTimeout(_that);case CrawlError_InvalidConfig() when invalidConfig != null:
return invalidConfig(_that);case CrawlError_Unsupported() when unsupported != null:
return unsupported(_that);case CrawlError_SsrfPolicyViolation() when ssrfPolicyViolation != null:
return ssrfPolicyViolation(_that);case CrawlError_Other() when other != null:
return other(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function( String field0)?  notFound,TResult Function( String field0)?  unauthorized,TResult Function( String field0)?  forbidden,TResult Function( String vendor,  String message)?  wafBlocked,TResult Function( String field0)?  timeout,TResult Function( String field0)?  rateLimited,TResult Function( String field0)?  serverError,TResult Function( String field0)?  badGateway,TResult Function( String field0)?  gone,TResult Function( String field0)?  connection,TResult Function( String field0)?  dns,TResult Function( String field0)?  ssl,TResult Function( String field0)?  dataLoss,TResult Function( String field0)?  browserError,TResult Function( String field0)?  browserTimeout,TResult Function( String field0)?  invalidConfig,TResult Function( String field0)?  unsupported,TResult Function( String url,  String reason)?  ssrfPolicyViolation,TResult Function( String field0)?  other,required TResult orElse(),}) {final _that = this;
switch (_that) {
case CrawlError_NotFound() when notFound != null:
return notFound(_that.field0);case CrawlError_Unauthorized() when unauthorized != null:
return unauthorized(_that.field0);case CrawlError_Forbidden() when forbidden != null:
return forbidden(_that.field0);case CrawlError_WafBlocked() when wafBlocked != null:
return wafBlocked(_that.vendor,_that.message);case CrawlError_Timeout() when timeout != null:
return timeout(_that.field0);case CrawlError_RateLimited() when rateLimited != null:
return rateLimited(_that.field0);case CrawlError_ServerError() when serverError != null:
return serverError(_that.field0);case CrawlError_BadGateway() when badGateway != null:
return badGateway(_that.field0);case CrawlError_Gone() when gone != null:
return gone(_that.field0);case CrawlError_Connection() when connection != null:
return connection(_that.field0);case CrawlError_Dns() when dns != null:
return dns(_that.field0);case CrawlError_Ssl() when ssl != null:
return ssl(_that.field0);case CrawlError_DataLoss() when dataLoss != null:
return dataLoss(_that.field0);case CrawlError_BrowserError() when browserError != null:
return browserError(_that.field0);case CrawlError_BrowserTimeout() when browserTimeout != null:
return browserTimeout(_that.field0);case CrawlError_InvalidConfig() when invalidConfig != null:
return invalidConfig(_that.field0);case CrawlError_Unsupported() when unsupported != null:
return unsupported(_that.field0);case CrawlError_SsrfPolicyViolation() when ssrfPolicyViolation != null:
return ssrfPolicyViolation(_that.url,_that.reason);case CrawlError_Other() when other != null:
return other(_that.field0);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function( String field0)  notFound,required TResult Function( String field0)  unauthorized,required TResult Function( String field0)  forbidden,required TResult Function( String vendor,  String message)  wafBlocked,required TResult Function( String field0)  timeout,required TResult Function( String field0)  rateLimited,required TResult Function( String field0)  serverError,required TResult Function( String field0)  badGateway,required TResult Function( String field0)  gone,required TResult Function( String field0)  connection,required TResult Function( String field0)  dns,required TResult Function( String field0)  ssl,required TResult Function( String field0)  dataLoss,required TResult Function( String field0)  browserError,required TResult Function( String field0)  browserTimeout,required TResult Function( String field0)  invalidConfig,required TResult Function( String field0)  unsupported,required TResult Function( String url,  String reason)  ssrfPolicyViolation,required TResult Function( String field0)  other,}) {final _that = this;
switch (_that) {
case CrawlError_NotFound():
return notFound(_that.field0);case CrawlError_Unauthorized():
return unauthorized(_that.field0);case CrawlError_Forbidden():
return forbidden(_that.field0);case CrawlError_WafBlocked():
return wafBlocked(_that.vendor,_that.message);case CrawlError_Timeout():
return timeout(_that.field0);case CrawlError_RateLimited():
return rateLimited(_that.field0);case CrawlError_ServerError():
return serverError(_that.field0);case CrawlError_BadGateway():
return badGateway(_that.field0);case CrawlError_Gone():
return gone(_that.field0);case CrawlError_Connection():
return connection(_that.field0);case CrawlError_Dns():
return dns(_that.field0);case CrawlError_Ssl():
return ssl(_that.field0);case CrawlError_DataLoss():
return dataLoss(_that.field0);case CrawlError_BrowserError():
return browserError(_that.field0);case CrawlError_BrowserTimeout():
return browserTimeout(_that.field0);case CrawlError_InvalidConfig():
return invalidConfig(_that.field0);case CrawlError_Unsupported():
return unsupported(_that.field0);case CrawlError_SsrfPolicyViolation():
return ssrfPolicyViolation(_that.url,_that.reason);case CrawlError_Other():
return other(_that.field0);}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function( String field0)?  notFound,TResult? Function( String field0)?  unauthorized,TResult? Function( String field0)?  forbidden,TResult? Function( String vendor,  String message)?  wafBlocked,TResult? Function( String field0)?  timeout,TResult? Function( String field0)?  rateLimited,TResult? Function( String field0)?  serverError,TResult? Function( String field0)?  badGateway,TResult? Function( String field0)?  gone,TResult? Function( String field0)?  connection,TResult? Function( String field0)?  dns,TResult? Function( String field0)?  ssl,TResult? Function( String field0)?  dataLoss,TResult? Function( String field0)?  browserError,TResult? Function( String field0)?  browserTimeout,TResult? Function( String field0)?  invalidConfig,TResult? Function( String field0)?  unsupported,TResult? Function( String url,  String reason)?  ssrfPolicyViolation,TResult? Function( String field0)?  other,}) {final _that = this;
switch (_that) {
case CrawlError_NotFound() when notFound != null:
return notFound(_that.field0);case CrawlError_Unauthorized() when unauthorized != null:
return unauthorized(_that.field0);case CrawlError_Forbidden() when forbidden != null:
return forbidden(_that.field0);case CrawlError_WafBlocked() when wafBlocked != null:
return wafBlocked(_that.vendor,_that.message);case CrawlError_Timeout() when timeout != null:
return timeout(_that.field0);case CrawlError_RateLimited() when rateLimited != null:
return rateLimited(_that.field0);case CrawlError_ServerError() when serverError != null:
return serverError(_that.field0);case CrawlError_BadGateway() when badGateway != null:
return badGateway(_that.field0);case CrawlError_Gone() when gone != null:
return gone(_that.field0);case CrawlError_Connection() when connection != null:
return connection(_that.field0);case CrawlError_Dns() when dns != null:
return dns(_that.field0);case CrawlError_Ssl() when ssl != null:
return ssl(_that.field0);case CrawlError_DataLoss() when dataLoss != null:
return dataLoss(_that.field0);case CrawlError_BrowserError() when browserError != null:
return browserError(_that.field0);case CrawlError_BrowserTimeout() when browserTimeout != null:
return browserTimeout(_that.field0);case CrawlError_InvalidConfig() when invalidConfig != null:
return invalidConfig(_that.field0);case CrawlError_Unsupported() when unsupported != null:
return unsupported(_that.field0);case CrawlError_SsrfPolicyViolation() when ssrfPolicyViolation != null:
return ssrfPolicyViolation(_that.url,_that.reason);case CrawlError_Other() when other != null:
return other(_that.field0);case _:
  return null;

}
}

}

/// @nodoc


class CrawlError_NotFound extends CrawlError {
  const CrawlError_NotFound({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_NotFoundCopyWith<CrawlError_NotFound> get copyWith => _$CrawlError_NotFoundCopyWithImpl<CrawlError_NotFound>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_NotFound&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.notFound(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_NotFoundCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_NotFoundCopyWith(CrawlError_NotFound value, $Res Function(CrawlError_NotFound) _then) = _$CrawlError_NotFoundCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_NotFoundCopyWithImpl<$Res>
    implements $CrawlError_NotFoundCopyWith<$Res> {
  _$CrawlError_NotFoundCopyWithImpl(this._self, this._then);

  final CrawlError_NotFound _self;
  final $Res Function(CrawlError_NotFound) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_NotFound(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_Unauthorized extends CrawlError {
  const CrawlError_Unauthorized({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_UnauthorizedCopyWith<CrawlError_Unauthorized> get copyWith => _$CrawlError_UnauthorizedCopyWithImpl<CrawlError_Unauthorized>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_Unauthorized&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.unauthorized(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_UnauthorizedCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_UnauthorizedCopyWith(CrawlError_Unauthorized value, $Res Function(CrawlError_Unauthorized) _then) = _$CrawlError_UnauthorizedCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_UnauthorizedCopyWithImpl<$Res>
    implements $CrawlError_UnauthorizedCopyWith<$Res> {
  _$CrawlError_UnauthorizedCopyWithImpl(this._self, this._then);

  final CrawlError_Unauthorized _self;
  final $Res Function(CrawlError_Unauthorized) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_Unauthorized(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_Forbidden extends CrawlError {
  const CrawlError_Forbidden({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_ForbiddenCopyWith<CrawlError_Forbidden> get copyWith => _$CrawlError_ForbiddenCopyWithImpl<CrawlError_Forbidden>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_Forbidden&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.forbidden(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_ForbiddenCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_ForbiddenCopyWith(CrawlError_Forbidden value, $Res Function(CrawlError_Forbidden) _then) = _$CrawlError_ForbiddenCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_ForbiddenCopyWithImpl<$Res>
    implements $CrawlError_ForbiddenCopyWith<$Res> {
  _$CrawlError_ForbiddenCopyWithImpl(this._self, this._then);

  final CrawlError_Forbidden _self;
  final $Res Function(CrawlError_Forbidden) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_Forbidden(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_WafBlocked extends CrawlError {
  const CrawlError_WafBlocked({required this.vendor, required this.message}): super._();
  

 final  String vendor;
 final  String message;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_WafBlockedCopyWith<CrawlError_WafBlocked> get copyWith => _$CrawlError_WafBlockedCopyWithImpl<CrawlError_WafBlocked>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_WafBlocked&&(identical(other.vendor, vendor) || other.vendor == vendor)&&(identical(other.message, message) || other.message == message));
}


@override
int get hashCode => Object.hash(runtimeType,vendor,message);

@override
String toString() {
  return 'CrawlError.wafBlocked(vendor: $vendor, message: $message)';
}


}

/// @nodoc
abstract mixin class $CrawlError_WafBlockedCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_WafBlockedCopyWith(CrawlError_WafBlocked value, $Res Function(CrawlError_WafBlocked) _then) = _$CrawlError_WafBlockedCopyWithImpl;
@useResult
$Res call({
 String vendor, String message
});




}
/// @nodoc
class _$CrawlError_WafBlockedCopyWithImpl<$Res>
    implements $CrawlError_WafBlockedCopyWith<$Res> {
  _$CrawlError_WafBlockedCopyWithImpl(this._self, this._then);

  final CrawlError_WafBlocked _self;
  final $Res Function(CrawlError_WafBlocked) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? vendor = null,Object? message = null,}) {
  return _then(CrawlError_WafBlocked(
vendor: null == vendor ? _self.vendor : vendor // ignore: cast_nullable_to_non_nullable
as String,message: null == message ? _self.message : message // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_Timeout extends CrawlError {
  const CrawlError_Timeout({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_TimeoutCopyWith<CrawlError_Timeout> get copyWith => _$CrawlError_TimeoutCopyWithImpl<CrawlError_Timeout>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_Timeout&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.timeout(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_TimeoutCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_TimeoutCopyWith(CrawlError_Timeout value, $Res Function(CrawlError_Timeout) _then) = _$CrawlError_TimeoutCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_TimeoutCopyWithImpl<$Res>
    implements $CrawlError_TimeoutCopyWith<$Res> {
  _$CrawlError_TimeoutCopyWithImpl(this._self, this._then);

  final CrawlError_Timeout _self;
  final $Res Function(CrawlError_Timeout) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_Timeout(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_RateLimited extends CrawlError {
  const CrawlError_RateLimited({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_RateLimitedCopyWith<CrawlError_RateLimited> get copyWith => _$CrawlError_RateLimitedCopyWithImpl<CrawlError_RateLimited>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_RateLimited&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.rateLimited(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_RateLimitedCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_RateLimitedCopyWith(CrawlError_RateLimited value, $Res Function(CrawlError_RateLimited) _then) = _$CrawlError_RateLimitedCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_RateLimitedCopyWithImpl<$Res>
    implements $CrawlError_RateLimitedCopyWith<$Res> {
  _$CrawlError_RateLimitedCopyWithImpl(this._self, this._then);

  final CrawlError_RateLimited _self;
  final $Res Function(CrawlError_RateLimited) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_RateLimited(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_ServerError extends CrawlError {
  const CrawlError_ServerError({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_ServerErrorCopyWith<CrawlError_ServerError> get copyWith => _$CrawlError_ServerErrorCopyWithImpl<CrawlError_ServerError>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_ServerError&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.serverError(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_ServerErrorCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_ServerErrorCopyWith(CrawlError_ServerError value, $Res Function(CrawlError_ServerError) _then) = _$CrawlError_ServerErrorCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_ServerErrorCopyWithImpl<$Res>
    implements $CrawlError_ServerErrorCopyWith<$Res> {
  _$CrawlError_ServerErrorCopyWithImpl(this._self, this._then);

  final CrawlError_ServerError _self;
  final $Res Function(CrawlError_ServerError) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_ServerError(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_BadGateway extends CrawlError {
  const CrawlError_BadGateway({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_BadGatewayCopyWith<CrawlError_BadGateway> get copyWith => _$CrawlError_BadGatewayCopyWithImpl<CrawlError_BadGateway>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_BadGateway&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.badGateway(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_BadGatewayCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_BadGatewayCopyWith(CrawlError_BadGateway value, $Res Function(CrawlError_BadGateway) _then) = _$CrawlError_BadGatewayCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_BadGatewayCopyWithImpl<$Res>
    implements $CrawlError_BadGatewayCopyWith<$Res> {
  _$CrawlError_BadGatewayCopyWithImpl(this._self, this._then);

  final CrawlError_BadGateway _self;
  final $Res Function(CrawlError_BadGateway) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_BadGateway(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_Gone extends CrawlError {
  const CrawlError_Gone({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_GoneCopyWith<CrawlError_Gone> get copyWith => _$CrawlError_GoneCopyWithImpl<CrawlError_Gone>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_Gone&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.gone(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_GoneCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_GoneCopyWith(CrawlError_Gone value, $Res Function(CrawlError_Gone) _then) = _$CrawlError_GoneCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_GoneCopyWithImpl<$Res>
    implements $CrawlError_GoneCopyWith<$Res> {
  _$CrawlError_GoneCopyWithImpl(this._self, this._then);

  final CrawlError_Gone _self;
  final $Res Function(CrawlError_Gone) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_Gone(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_Connection extends CrawlError {
  const CrawlError_Connection({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_ConnectionCopyWith<CrawlError_Connection> get copyWith => _$CrawlError_ConnectionCopyWithImpl<CrawlError_Connection>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_Connection&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.connection(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_ConnectionCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_ConnectionCopyWith(CrawlError_Connection value, $Res Function(CrawlError_Connection) _then) = _$CrawlError_ConnectionCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_ConnectionCopyWithImpl<$Res>
    implements $CrawlError_ConnectionCopyWith<$Res> {
  _$CrawlError_ConnectionCopyWithImpl(this._self, this._then);

  final CrawlError_Connection _self;
  final $Res Function(CrawlError_Connection) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_Connection(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_Dns extends CrawlError {
  const CrawlError_Dns({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_DnsCopyWith<CrawlError_Dns> get copyWith => _$CrawlError_DnsCopyWithImpl<CrawlError_Dns>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_Dns&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.dns(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_DnsCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_DnsCopyWith(CrawlError_Dns value, $Res Function(CrawlError_Dns) _then) = _$CrawlError_DnsCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_DnsCopyWithImpl<$Res>
    implements $CrawlError_DnsCopyWith<$Res> {
  _$CrawlError_DnsCopyWithImpl(this._self, this._then);

  final CrawlError_Dns _self;
  final $Res Function(CrawlError_Dns) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_Dns(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_Ssl extends CrawlError {
  const CrawlError_Ssl({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_SslCopyWith<CrawlError_Ssl> get copyWith => _$CrawlError_SslCopyWithImpl<CrawlError_Ssl>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_Ssl&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.ssl(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_SslCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_SslCopyWith(CrawlError_Ssl value, $Res Function(CrawlError_Ssl) _then) = _$CrawlError_SslCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_SslCopyWithImpl<$Res>
    implements $CrawlError_SslCopyWith<$Res> {
  _$CrawlError_SslCopyWithImpl(this._self, this._then);

  final CrawlError_Ssl _self;
  final $Res Function(CrawlError_Ssl) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_Ssl(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_DataLoss extends CrawlError {
  const CrawlError_DataLoss({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_DataLossCopyWith<CrawlError_DataLoss> get copyWith => _$CrawlError_DataLossCopyWithImpl<CrawlError_DataLoss>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_DataLoss&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.dataLoss(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_DataLossCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_DataLossCopyWith(CrawlError_DataLoss value, $Res Function(CrawlError_DataLoss) _then) = _$CrawlError_DataLossCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_DataLossCopyWithImpl<$Res>
    implements $CrawlError_DataLossCopyWith<$Res> {
  _$CrawlError_DataLossCopyWithImpl(this._self, this._then);

  final CrawlError_DataLoss _self;
  final $Res Function(CrawlError_DataLoss) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_DataLoss(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_BrowserError extends CrawlError {
  const CrawlError_BrowserError({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_BrowserErrorCopyWith<CrawlError_BrowserError> get copyWith => _$CrawlError_BrowserErrorCopyWithImpl<CrawlError_BrowserError>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_BrowserError&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.browserError(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_BrowserErrorCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_BrowserErrorCopyWith(CrawlError_BrowserError value, $Res Function(CrawlError_BrowserError) _then) = _$CrawlError_BrowserErrorCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_BrowserErrorCopyWithImpl<$Res>
    implements $CrawlError_BrowserErrorCopyWith<$Res> {
  _$CrawlError_BrowserErrorCopyWithImpl(this._self, this._then);

  final CrawlError_BrowserError _self;
  final $Res Function(CrawlError_BrowserError) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_BrowserError(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_BrowserTimeout extends CrawlError {
  const CrawlError_BrowserTimeout({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_BrowserTimeoutCopyWith<CrawlError_BrowserTimeout> get copyWith => _$CrawlError_BrowserTimeoutCopyWithImpl<CrawlError_BrowserTimeout>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_BrowserTimeout&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.browserTimeout(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_BrowserTimeoutCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_BrowserTimeoutCopyWith(CrawlError_BrowserTimeout value, $Res Function(CrawlError_BrowserTimeout) _then) = _$CrawlError_BrowserTimeoutCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_BrowserTimeoutCopyWithImpl<$Res>
    implements $CrawlError_BrowserTimeoutCopyWith<$Res> {
  _$CrawlError_BrowserTimeoutCopyWithImpl(this._self, this._then);

  final CrawlError_BrowserTimeout _self;
  final $Res Function(CrawlError_BrowserTimeout) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_BrowserTimeout(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_InvalidConfig extends CrawlError {
  const CrawlError_InvalidConfig({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_InvalidConfigCopyWith<CrawlError_InvalidConfig> get copyWith => _$CrawlError_InvalidConfigCopyWithImpl<CrawlError_InvalidConfig>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_InvalidConfig&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.invalidConfig(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_InvalidConfigCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_InvalidConfigCopyWith(CrawlError_InvalidConfig value, $Res Function(CrawlError_InvalidConfig) _then) = _$CrawlError_InvalidConfigCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_InvalidConfigCopyWithImpl<$Res>
    implements $CrawlError_InvalidConfigCopyWith<$Res> {
  _$CrawlError_InvalidConfigCopyWithImpl(this._self, this._then);

  final CrawlError_InvalidConfig _self;
  final $Res Function(CrawlError_InvalidConfig) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_InvalidConfig(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_Unsupported extends CrawlError {
  const CrawlError_Unsupported({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_UnsupportedCopyWith<CrawlError_Unsupported> get copyWith => _$CrawlError_UnsupportedCopyWithImpl<CrawlError_Unsupported>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_Unsupported&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.unsupported(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_UnsupportedCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_UnsupportedCopyWith(CrawlError_Unsupported value, $Res Function(CrawlError_Unsupported) _then) = _$CrawlError_UnsupportedCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_UnsupportedCopyWithImpl<$Res>
    implements $CrawlError_UnsupportedCopyWith<$Res> {
  _$CrawlError_UnsupportedCopyWithImpl(this._self, this._then);

  final CrawlError_Unsupported _self;
  final $Res Function(CrawlError_Unsupported) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_Unsupported(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_SsrfPolicyViolation extends CrawlError {
  const CrawlError_SsrfPolicyViolation({required this.url, required this.reason}): super._();
  

 final  String url;
 final  String reason;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_SsrfPolicyViolationCopyWith<CrawlError_SsrfPolicyViolation> get copyWith => _$CrawlError_SsrfPolicyViolationCopyWithImpl<CrawlError_SsrfPolicyViolation>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_SsrfPolicyViolation&&(identical(other.url, url) || other.url == url)&&(identical(other.reason, reason) || other.reason == reason));
}


@override
int get hashCode => Object.hash(runtimeType,url,reason);

@override
String toString() {
  return 'CrawlError.ssrfPolicyViolation(url: $url, reason: $reason)';
}


}

/// @nodoc
abstract mixin class $CrawlError_SsrfPolicyViolationCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_SsrfPolicyViolationCopyWith(CrawlError_SsrfPolicyViolation value, $Res Function(CrawlError_SsrfPolicyViolation) _then) = _$CrawlError_SsrfPolicyViolationCopyWithImpl;
@useResult
$Res call({
 String url, String reason
});




}
/// @nodoc
class _$CrawlError_SsrfPolicyViolationCopyWithImpl<$Res>
    implements $CrawlError_SsrfPolicyViolationCopyWith<$Res> {
  _$CrawlError_SsrfPolicyViolationCopyWithImpl(this._self, this._then);

  final CrawlError_SsrfPolicyViolation _self;
  final $Res Function(CrawlError_SsrfPolicyViolation) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? url = null,Object? reason = null,}) {
  return _then(CrawlError_SsrfPolicyViolation(
url: null == url ? _self.url : url // ignore: cast_nullable_to_non_nullable
as String,reason: null == reason ? _self.reason : reason // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlError_Other extends CrawlError {
  const CrawlError_Other({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlError_OtherCopyWith<CrawlError_Other> get copyWith => _$CrawlError_OtherCopyWithImpl<CrawlError_Other>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlError_Other&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'CrawlError.other(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $CrawlError_OtherCopyWith<$Res> implements $CrawlErrorCopyWith<$Res> {
  factory $CrawlError_OtherCopyWith(CrawlError_Other value, $Res Function(CrawlError_Other) _then) = _$CrawlError_OtherCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$CrawlError_OtherCopyWithImpl<$Res>
    implements $CrawlError_OtherCopyWith<$Res> {
  _$CrawlError_OtherCopyWithImpl(this._self, this._then);

  final CrawlError_Other _self;
  final $Res Function(CrawlError_Other) _then;

/// Create a copy of CrawlError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(CrawlError_Other(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc
mixin _$CrawlEvent {





@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlEvent);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
  return 'CrawlEvent()';
}


}

/// @nodoc
class $CrawlEventCopyWith<$Res>  {
$CrawlEventCopyWith(CrawlEvent _, $Res Function(CrawlEvent) __);
}


/// Adds pattern-matching-related methods to [CrawlEvent].
extension CrawlEventPatterns on CrawlEvent {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( CrawlEvent_Page value)?  page,TResult Function( CrawlEvent_Error value)?  error,TResult Function( CrawlEvent_Complete value)?  complete,required TResult orElse(),}){
final _that = this;
switch (_that) {
case CrawlEvent_Page() when page != null:
return page(_that);case CrawlEvent_Error() when error != null:
return error(_that);case CrawlEvent_Complete() when complete != null:
return complete(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( CrawlEvent_Page value)  page,required TResult Function( CrawlEvent_Error value)  error,required TResult Function( CrawlEvent_Complete value)  complete,}){
final _that = this;
switch (_that) {
case CrawlEvent_Page():
return page(_that);case CrawlEvent_Error():
return error(_that);case CrawlEvent_Complete():
return complete(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( CrawlEvent_Page value)?  page,TResult? Function( CrawlEvent_Error value)?  error,TResult? Function( CrawlEvent_Complete value)?  complete,}){
final _that = this;
switch (_that) {
case CrawlEvent_Page() when page != null:
return page(_that);case CrawlEvent_Error() when error != null:
return error(_that);case CrawlEvent_Complete() when complete != null:
return complete(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function( CrawlPageResult result)?  page,TResult Function( String url,  String error)?  error,TResult Function( PlatformInt64 pagesCrawled)?  complete,required TResult orElse(),}) {final _that = this;
switch (_that) {
case CrawlEvent_Page() when page != null:
return page(_that.result);case CrawlEvent_Error() when error != null:
return error(_that.url,_that.error);case CrawlEvent_Complete() when complete != null:
return complete(_that.pagesCrawled);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function( CrawlPageResult result)  page,required TResult Function( String url,  String error)  error,required TResult Function( PlatformInt64 pagesCrawled)  complete,}) {final _that = this;
switch (_that) {
case CrawlEvent_Page():
return page(_that.result);case CrawlEvent_Error():
return error(_that.url,_that.error);case CrawlEvent_Complete():
return complete(_that.pagesCrawled);}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function( CrawlPageResult result)?  page,TResult? Function( String url,  String error)?  error,TResult? Function( PlatformInt64 pagesCrawled)?  complete,}) {final _that = this;
switch (_that) {
case CrawlEvent_Page() when page != null:
return page(_that.result);case CrawlEvent_Error() when error != null:
return error(_that.url,_that.error);case CrawlEvent_Complete() when complete != null:
return complete(_that.pagesCrawled);case _:
  return null;

}
}

}

/// @nodoc


class CrawlEvent_Page extends CrawlEvent {
  const CrawlEvent_Page({required this.result}): super._();
  

/// The crawled page result.
 final  CrawlPageResult result;

/// Create a copy of CrawlEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlEvent_PageCopyWith<CrawlEvent_Page> get copyWith => _$CrawlEvent_PageCopyWithImpl<CrawlEvent_Page>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlEvent_Page&&(identical(other.result, result) || other.result == result));
}


@override
int get hashCode => Object.hash(runtimeType,result);

@override
String toString() {
  return 'CrawlEvent.page(result: $result)';
}


}

/// @nodoc
abstract mixin class $CrawlEvent_PageCopyWith<$Res> implements $CrawlEventCopyWith<$Res> {
  factory $CrawlEvent_PageCopyWith(CrawlEvent_Page value, $Res Function(CrawlEvent_Page) _then) = _$CrawlEvent_PageCopyWithImpl;
@useResult
$Res call({
 CrawlPageResult result
});




}
/// @nodoc
class _$CrawlEvent_PageCopyWithImpl<$Res>
    implements $CrawlEvent_PageCopyWith<$Res> {
  _$CrawlEvent_PageCopyWithImpl(this._self, this._then);

  final CrawlEvent_Page _self;
  final $Res Function(CrawlEvent_Page) _then;

/// Create a copy of CrawlEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? result = null,}) {
  return _then(CrawlEvent_Page(
result: null == result ? _self.result : result // ignore: cast_nullable_to_non_nullable
as CrawlPageResult,
  ));
}


}

/// @nodoc


class CrawlEvent_Error extends CrawlEvent {
  const CrawlEvent_Error({required this.url, required this.error}): super._();
  

/// The URL that failed.
 final  String url;
/// The error message.
 final  String error;

/// Create a copy of CrawlEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlEvent_ErrorCopyWith<CrawlEvent_Error> get copyWith => _$CrawlEvent_ErrorCopyWithImpl<CrawlEvent_Error>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlEvent_Error&&(identical(other.url, url) || other.url == url)&&(identical(other.error, error) || other.error == error));
}


@override
int get hashCode => Object.hash(runtimeType,url,error);

@override
String toString() {
  return 'CrawlEvent.error(url: $url, error: $error)';
}


}

/// @nodoc
abstract mixin class $CrawlEvent_ErrorCopyWith<$Res> implements $CrawlEventCopyWith<$Res> {
  factory $CrawlEvent_ErrorCopyWith(CrawlEvent_Error value, $Res Function(CrawlEvent_Error) _then) = _$CrawlEvent_ErrorCopyWithImpl;
@useResult
$Res call({
 String url, String error
});




}
/// @nodoc
class _$CrawlEvent_ErrorCopyWithImpl<$Res>
    implements $CrawlEvent_ErrorCopyWith<$Res> {
  _$CrawlEvent_ErrorCopyWithImpl(this._self, this._then);

  final CrawlEvent_Error _self;
  final $Res Function(CrawlEvent_Error) _then;

/// Create a copy of CrawlEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? url = null,Object? error = null,}) {
  return _then(CrawlEvent_Error(
url: null == url ? _self.url : url // ignore: cast_nullable_to_non_nullable
as String,error: null == error ? _self.error : error // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class CrawlEvent_Complete extends CrawlEvent {
  const CrawlEvent_Complete({required this.pagesCrawled}): super._();
  

/// Total number of pages crawled.
 final  PlatformInt64 pagesCrawled;

/// Create a copy of CrawlEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CrawlEvent_CompleteCopyWith<CrawlEvent_Complete> get copyWith => _$CrawlEvent_CompleteCopyWithImpl<CrawlEvent_Complete>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is CrawlEvent_Complete&&(identical(other.pagesCrawled, pagesCrawled) || other.pagesCrawled == pagesCrawled));
}


@override
int get hashCode => Object.hash(runtimeType,pagesCrawled);

@override
String toString() {
  return 'CrawlEvent.complete(pagesCrawled: $pagesCrawled)';
}


}

/// @nodoc
abstract mixin class $CrawlEvent_CompleteCopyWith<$Res> implements $CrawlEventCopyWith<$Res> {
  factory $CrawlEvent_CompleteCopyWith(CrawlEvent_Complete value, $Res Function(CrawlEvent_Complete) _then) = _$CrawlEvent_CompleteCopyWithImpl;
@useResult
$Res call({
 PlatformInt64 pagesCrawled
});




}
/// @nodoc
class _$CrawlEvent_CompleteCopyWithImpl<$Res>
    implements $CrawlEvent_CompleteCopyWith<$Res> {
  _$CrawlEvent_CompleteCopyWithImpl(this._self, this._then);

  final CrawlEvent_Complete _self;
  final $Res Function(CrawlEvent_Complete) _then;

/// Create a copy of CrawlEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? pagesCrawled = null,}) {
  return _then(CrawlEvent_Complete(
pagesCrawled: null == pagesCrawled ? _self.pagesCrawled : pagesCrawled // ignore: cast_nullable_to_non_nullable
as PlatformInt64,
  ));
}


}

/// @nodoc
mixin _$PageAction {





@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is PageAction);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
  return 'PageAction()';
}


}

/// @nodoc
class $PageActionCopyWith<$Res>  {
$PageActionCopyWith(PageAction _, $Res Function(PageAction) __);
}


/// Adds pattern-matching-related methods to [PageAction].
extension PageActionPatterns on PageAction {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( PageAction_Click value)?  click,TResult Function( PageAction_TypeText value)?  typeText,TResult Function( PageAction_Press value)?  press,TResult Function( PageAction_Scroll value)?  scroll,TResult Function( PageAction_Wait value)?  wait,TResult Function( PageAction_Screenshot value)?  screenshot,TResult Function( PageAction_ExecuteJs value)?  executeJs,TResult Function( PageAction_Scrape value)?  scrape,required TResult orElse(),}){
final _that = this;
switch (_that) {
case PageAction_Click() when click != null:
return click(_that);case PageAction_TypeText() when typeText != null:
return typeText(_that);case PageAction_Press() when press != null:
return press(_that);case PageAction_Scroll() when scroll != null:
return scroll(_that);case PageAction_Wait() when wait != null:
return wait(_that);case PageAction_Screenshot() when screenshot != null:
return screenshot(_that);case PageAction_ExecuteJs() when executeJs != null:
return executeJs(_that);case PageAction_Scrape() when scrape != null:
return scrape(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( PageAction_Click value)  click,required TResult Function( PageAction_TypeText value)  typeText,required TResult Function( PageAction_Press value)  press,required TResult Function( PageAction_Scroll value)  scroll,required TResult Function( PageAction_Wait value)  wait,required TResult Function( PageAction_Screenshot value)  screenshot,required TResult Function( PageAction_ExecuteJs value)  executeJs,required TResult Function( PageAction_Scrape value)  scrape,}){
final _that = this;
switch (_that) {
case PageAction_Click():
return click(_that);case PageAction_TypeText():
return typeText(_that);case PageAction_Press():
return press(_that);case PageAction_Scroll():
return scroll(_that);case PageAction_Wait():
return wait(_that);case PageAction_Screenshot():
return screenshot(_that);case PageAction_ExecuteJs():
return executeJs(_that);case PageAction_Scrape():
return scrape(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( PageAction_Click value)?  click,TResult? Function( PageAction_TypeText value)?  typeText,TResult? Function( PageAction_Press value)?  press,TResult? Function( PageAction_Scroll value)?  scroll,TResult? Function( PageAction_Wait value)?  wait,TResult? Function( PageAction_Screenshot value)?  screenshot,TResult? Function( PageAction_ExecuteJs value)?  executeJs,TResult? Function( PageAction_Scrape value)?  scrape,}){
final _that = this;
switch (_that) {
case PageAction_Click() when click != null:
return click(_that);case PageAction_TypeText() when typeText != null:
return typeText(_that);case PageAction_Press() when press != null:
return press(_that);case PageAction_Scroll() when scroll != null:
return scroll(_that);case PageAction_Wait() when wait != null:
return wait(_that);case PageAction_Screenshot() when screenshot != null:
return screenshot(_that);case PageAction_ExecuteJs() when executeJs != null:
return executeJs(_that);case PageAction_Scrape() when scrape != null:
return scrape(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function( String selector)?  click,TResult Function( String selector,  String text)?  typeText,TResult Function( String key)?  press,TResult Function( ScrollDirection direction,  String selector,  PlatformInt64 amount)?  scroll,TResult Function( PlatformInt64 milliseconds,  String selector)?  wait,TResult Function( bool fullPage)?  screenshot,TResult Function( String script)?  executeJs,TResult Function()?  scrape,required TResult orElse(),}) {final _that = this;
switch (_that) {
case PageAction_Click() when click != null:
return click(_that.selector);case PageAction_TypeText() when typeText != null:
return typeText(_that.selector,_that.text);case PageAction_Press() when press != null:
return press(_that.key);case PageAction_Scroll() when scroll != null:
return scroll(_that.direction,_that.selector,_that.amount);case PageAction_Wait() when wait != null:
return wait(_that.milliseconds,_that.selector);case PageAction_Screenshot() when screenshot != null:
return screenshot(_that.fullPage);case PageAction_ExecuteJs() when executeJs != null:
return executeJs(_that.script);case PageAction_Scrape() when scrape != null:
return scrape();case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function( String selector)  click,required TResult Function( String selector,  String text)  typeText,required TResult Function( String key)  press,required TResult Function( ScrollDirection direction,  String selector,  PlatformInt64 amount)  scroll,required TResult Function( PlatformInt64 milliseconds,  String selector)  wait,required TResult Function( bool fullPage)  screenshot,required TResult Function( String script)  executeJs,required TResult Function()  scrape,}) {final _that = this;
switch (_that) {
case PageAction_Click():
return click(_that.selector);case PageAction_TypeText():
return typeText(_that.selector,_that.text);case PageAction_Press():
return press(_that.key);case PageAction_Scroll():
return scroll(_that.direction,_that.selector,_that.amount);case PageAction_Wait():
return wait(_that.milliseconds,_that.selector);case PageAction_Screenshot():
return screenshot(_that.fullPage);case PageAction_ExecuteJs():
return executeJs(_that.script);case PageAction_Scrape():
return scrape();}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function( String selector)?  click,TResult? Function( String selector,  String text)?  typeText,TResult? Function( String key)?  press,TResult? Function( ScrollDirection direction,  String selector,  PlatformInt64 amount)?  scroll,TResult? Function( PlatformInt64 milliseconds,  String selector)?  wait,TResult? Function( bool fullPage)?  screenshot,TResult? Function( String script)?  executeJs,TResult? Function()?  scrape,}) {final _that = this;
switch (_that) {
case PageAction_Click() when click != null:
return click(_that.selector);case PageAction_TypeText() when typeText != null:
return typeText(_that.selector,_that.text);case PageAction_Press() when press != null:
return press(_that.key);case PageAction_Scroll() when scroll != null:
return scroll(_that.direction,_that.selector,_that.amount);case PageAction_Wait() when wait != null:
return wait(_that.milliseconds,_that.selector);case PageAction_Screenshot() when screenshot != null:
return screenshot(_that.fullPage);case PageAction_ExecuteJs() when executeJs != null:
return executeJs(_that.script);case PageAction_Scrape() when scrape != null:
return scrape();case _:
  return null;

}
}

}

/// @nodoc


class PageAction_Click extends PageAction {
  const PageAction_Click({required this.selector}): super._();
  

/// CSS selector for the element to click.
 final  String selector;

/// Create a copy of PageAction
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$PageAction_ClickCopyWith<PageAction_Click> get copyWith => _$PageAction_ClickCopyWithImpl<PageAction_Click>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is PageAction_Click&&(identical(other.selector, selector) || other.selector == selector));
}


@override
int get hashCode => Object.hash(runtimeType,selector);

@override
String toString() {
  return 'PageAction.click(selector: $selector)';
}


}

/// @nodoc
abstract mixin class $PageAction_ClickCopyWith<$Res> implements $PageActionCopyWith<$Res> {
  factory $PageAction_ClickCopyWith(PageAction_Click value, $Res Function(PageAction_Click) _then) = _$PageAction_ClickCopyWithImpl;
@useResult
$Res call({
 String selector
});




}
/// @nodoc
class _$PageAction_ClickCopyWithImpl<$Res>
    implements $PageAction_ClickCopyWith<$Res> {
  _$PageAction_ClickCopyWithImpl(this._self, this._then);

  final PageAction_Click _self;
  final $Res Function(PageAction_Click) _then;

/// Create a copy of PageAction
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? selector = null,}) {
  return _then(PageAction_Click(
selector: null == selector ? _self.selector : selector // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class PageAction_TypeText extends PageAction {
  const PageAction_TypeText({required this.selector, required this.text}): super._();
  

/// CSS selector for the input element.
 final  String selector;
/// Text to type into the element.
 final  String text;

/// Create a copy of PageAction
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$PageAction_TypeTextCopyWith<PageAction_TypeText> get copyWith => _$PageAction_TypeTextCopyWithImpl<PageAction_TypeText>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is PageAction_TypeText&&(identical(other.selector, selector) || other.selector == selector)&&(identical(other.text, text) || other.text == text));
}


@override
int get hashCode => Object.hash(runtimeType,selector,text);

@override
String toString() {
  return 'PageAction.typeText(selector: $selector, text: $text)';
}


}

/// @nodoc
abstract mixin class $PageAction_TypeTextCopyWith<$Res> implements $PageActionCopyWith<$Res> {
  factory $PageAction_TypeTextCopyWith(PageAction_TypeText value, $Res Function(PageAction_TypeText) _then) = _$PageAction_TypeTextCopyWithImpl;
@useResult
$Res call({
 String selector, String text
});




}
/// @nodoc
class _$PageAction_TypeTextCopyWithImpl<$Res>
    implements $PageAction_TypeTextCopyWith<$Res> {
  _$PageAction_TypeTextCopyWithImpl(this._self, this._then);

  final PageAction_TypeText _self;
  final $Res Function(PageAction_TypeText) _then;

/// Create a copy of PageAction
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? selector = null,Object? text = null,}) {
  return _then(PageAction_TypeText(
selector: null == selector ? _self.selector : selector // ignore: cast_nullable_to_non_nullable
as String,text: null == text ? _self.text : text // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class PageAction_Press extends PageAction {
  const PageAction_Press({required this.key}): super._();
  

/// Key name to press.
 final  String key;

/// Create a copy of PageAction
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$PageAction_PressCopyWith<PageAction_Press> get copyWith => _$PageAction_PressCopyWithImpl<PageAction_Press>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is PageAction_Press&&(identical(other.key, key) || other.key == key));
}


@override
int get hashCode => Object.hash(runtimeType,key);

@override
String toString() {
  return 'PageAction.press(key: $key)';
}


}

/// @nodoc
abstract mixin class $PageAction_PressCopyWith<$Res> implements $PageActionCopyWith<$Res> {
  factory $PageAction_PressCopyWith(PageAction_Press value, $Res Function(PageAction_Press) _then) = _$PageAction_PressCopyWithImpl;
@useResult
$Res call({
 String key
});




}
/// @nodoc
class _$PageAction_PressCopyWithImpl<$Res>
    implements $PageAction_PressCopyWith<$Res> {
  _$PageAction_PressCopyWithImpl(this._self, this._then);

  final PageAction_Press _self;
  final $Res Function(PageAction_Press) _then;

/// Create a copy of PageAction
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? key = null,}) {
  return _then(PageAction_Press(
key: null == key ? _self.key : key // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class PageAction_Scroll extends PageAction {
  const PageAction_Scroll({required this.direction, required this.selector, required this.amount}): super._();
  

/// Direction to scroll.
 final  ScrollDirection direction;
/// Optional CSS selector for a scrollable element. Scrolls the page if absent.
 final  String selector;
/// Optional pixel amount to scroll. Uses a default if absent.
 final  PlatformInt64 amount;

/// Create a copy of PageAction
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$PageAction_ScrollCopyWith<PageAction_Scroll> get copyWith => _$PageAction_ScrollCopyWithImpl<PageAction_Scroll>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is PageAction_Scroll&&(identical(other.direction, direction) || other.direction == direction)&&(identical(other.selector, selector) || other.selector == selector)&&(identical(other.amount, amount) || other.amount == amount));
}


@override
int get hashCode => Object.hash(runtimeType,direction,selector,amount);

@override
String toString() {
  return 'PageAction.scroll(direction: $direction, selector: $selector, amount: $amount)';
}


}

/// @nodoc
abstract mixin class $PageAction_ScrollCopyWith<$Res> implements $PageActionCopyWith<$Res> {
  factory $PageAction_ScrollCopyWith(PageAction_Scroll value, $Res Function(PageAction_Scroll) _then) = _$PageAction_ScrollCopyWithImpl;
@useResult
$Res call({
 ScrollDirection direction, String selector, PlatformInt64 amount
});




}
/// @nodoc
class _$PageAction_ScrollCopyWithImpl<$Res>
    implements $PageAction_ScrollCopyWith<$Res> {
  _$PageAction_ScrollCopyWithImpl(this._self, this._then);

  final PageAction_Scroll _self;
  final $Res Function(PageAction_Scroll) _then;

/// Create a copy of PageAction
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? direction = null,Object? selector = null,Object? amount = null,}) {
  return _then(PageAction_Scroll(
direction: null == direction ? _self.direction : direction // ignore: cast_nullable_to_non_nullable
as ScrollDirection,selector: null == selector ? _self.selector : selector // ignore: cast_nullable_to_non_nullable
as String,amount: null == amount ? _self.amount : amount // ignore: cast_nullable_to_non_nullable
as PlatformInt64,
  ));
}


}

/// @nodoc


class PageAction_Wait extends PageAction {
  const PageAction_Wait({required this.milliseconds, required this.selector}): super._();
  

/// Milliseconds to wait. Ignored if `selector` is provided.
 final  PlatformInt64 milliseconds;
/// CSS selector to wait for.
 final  String selector;

/// Create a copy of PageAction
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$PageAction_WaitCopyWith<PageAction_Wait> get copyWith => _$PageAction_WaitCopyWithImpl<PageAction_Wait>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is PageAction_Wait&&(identical(other.milliseconds, milliseconds) || other.milliseconds == milliseconds)&&(identical(other.selector, selector) || other.selector == selector));
}


@override
int get hashCode => Object.hash(runtimeType,milliseconds,selector);

@override
String toString() {
  return 'PageAction.wait(milliseconds: $milliseconds, selector: $selector)';
}


}

/// @nodoc
abstract mixin class $PageAction_WaitCopyWith<$Res> implements $PageActionCopyWith<$Res> {
  factory $PageAction_WaitCopyWith(PageAction_Wait value, $Res Function(PageAction_Wait) _then) = _$PageAction_WaitCopyWithImpl;
@useResult
$Res call({
 PlatformInt64 milliseconds, String selector
});




}
/// @nodoc
class _$PageAction_WaitCopyWithImpl<$Res>
    implements $PageAction_WaitCopyWith<$Res> {
  _$PageAction_WaitCopyWithImpl(this._self, this._then);

  final PageAction_Wait _self;
  final $Res Function(PageAction_Wait) _then;

/// Create a copy of PageAction
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? milliseconds = null,Object? selector = null,}) {
  return _then(PageAction_Wait(
milliseconds: null == milliseconds ? _self.milliseconds : milliseconds // ignore: cast_nullable_to_non_nullable
as PlatformInt64,selector: null == selector ? _self.selector : selector // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class PageAction_Screenshot extends PageAction {
  const PageAction_Screenshot({required this.fullPage}): super._();
  

/// Whether to capture the full scrollable page. Defaults to viewport only.
///
/// Accepts both the canonical `fullPage` (camelCase) form and the
/// `full_page` (snake_case) alias so language bindings and fixtures can
/// use either convention without error.
 final  bool fullPage;

/// Create a copy of PageAction
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$PageAction_ScreenshotCopyWith<PageAction_Screenshot> get copyWith => _$PageAction_ScreenshotCopyWithImpl<PageAction_Screenshot>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is PageAction_Screenshot&&(identical(other.fullPage, fullPage) || other.fullPage == fullPage));
}


@override
int get hashCode => Object.hash(runtimeType,fullPage);

@override
String toString() {
  return 'PageAction.screenshot(fullPage: $fullPage)';
}


}

/// @nodoc
abstract mixin class $PageAction_ScreenshotCopyWith<$Res> implements $PageActionCopyWith<$Res> {
  factory $PageAction_ScreenshotCopyWith(PageAction_Screenshot value, $Res Function(PageAction_Screenshot) _then) = _$PageAction_ScreenshotCopyWithImpl;
@useResult
$Res call({
 bool fullPage
});




}
/// @nodoc
class _$PageAction_ScreenshotCopyWithImpl<$Res>
    implements $PageAction_ScreenshotCopyWith<$Res> {
  _$PageAction_ScreenshotCopyWithImpl(this._self, this._then);

  final PageAction_Screenshot _self;
  final $Res Function(PageAction_Screenshot) _then;

/// Create a copy of PageAction
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? fullPage = null,}) {
  return _then(PageAction_Screenshot(
fullPage: null == fullPage ? _self.fullPage : fullPage // ignore: cast_nullable_to_non_nullable
as bool,
  ));
}


}

/// @nodoc


class PageAction_ExecuteJs extends PageAction {
  const PageAction_ExecuteJs({required this.script}): super._();
  

/// JavaScript source code to execute. Max 1 MB.
 final  String script;

/// Create a copy of PageAction
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$PageAction_ExecuteJsCopyWith<PageAction_ExecuteJs> get copyWith => _$PageAction_ExecuteJsCopyWithImpl<PageAction_ExecuteJs>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is PageAction_ExecuteJs&&(identical(other.script, script) || other.script == script));
}


@override
int get hashCode => Object.hash(runtimeType,script);

@override
String toString() {
  return 'PageAction.executeJs(script: $script)';
}


}

/// @nodoc
abstract mixin class $PageAction_ExecuteJsCopyWith<$Res> implements $PageActionCopyWith<$Res> {
  factory $PageAction_ExecuteJsCopyWith(PageAction_ExecuteJs value, $Res Function(PageAction_ExecuteJs) _then) = _$PageAction_ExecuteJsCopyWithImpl;
@useResult
$Res call({
 String script
});




}
/// @nodoc
class _$PageAction_ExecuteJsCopyWithImpl<$Res>
    implements $PageAction_ExecuteJsCopyWith<$Res> {
  _$PageAction_ExecuteJsCopyWithImpl(this._self, this._then);

  final PageAction_ExecuteJs _self;
  final $Res Function(PageAction_ExecuteJs) _then;

/// Create a copy of PageAction
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? script = null,}) {
  return _then(PageAction_ExecuteJs(
script: null == script ? _self.script : script // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class PageAction_Scrape extends PageAction {
  const PageAction_Scrape(): super._();
  






@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is PageAction_Scrape);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
  return 'PageAction.scrape()';
}


}




/// @nodoc
mixin _$SsrfError {





@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is SsrfError);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
  return 'SsrfError()';
}


}

/// @nodoc
class $SsrfErrorCopyWith<$Res>  {
$SsrfErrorCopyWith(SsrfError _, $Res Function(SsrfError) __);
}


/// Adds pattern-matching-related methods to [SsrfError].
extension SsrfErrorPatterns on SsrfError {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( SsrfError_DeniedByPolicy value)?  deniedByPolicy,TResult Function( SsrfError_NotOnAllowlist value)?  notOnAllowlist,TResult Function( SsrfError_DnsResolutionFailed value)?  dnsResolutionFailed,TResult Function( SsrfError_InvalidUrl value)?  invalidUrl,TResult Function( SsrfError_DisallowedScheme value)?  disallowedScheme,TResult Function( SsrfError_TooManyRedirects value)?  tooManyRedirects,required TResult orElse(),}){
final _that = this;
switch (_that) {
case SsrfError_DeniedByPolicy() when deniedByPolicy != null:
return deniedByPolicy(_that);case SsrfError_NotOnAllowlist() when notOnAllowlist != null:
return notOnAllowlist(_that);case SsrfError_DnsResolutionFailed() when dnsResolutionFailed != null:
return dnsResolutionFailed(_that);case SsrfError_InvalidUrl() when invalidUrl != null:
return invalidUrl(_that);case SsrfError_DisallowedScheme() when disallowedScheme != null:
return disallowedScheme(_that);case SsrfError_TooManyRedirects() when tooManyRedirects != null:
return tooManyRedirects(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( SsrfError_DeniedByPolicy value)  deniedByPolicy,required TResult Function( SsrfError_NotOnAllowlist value)  notOnAllowlist,required TResult Function( SsrfError_DnsResolutionFailed value)  dnsResolutionFailed,required TResult Function( SsrfError_InvalidUrl value)  invalidUrl,required TResult Function( SsrfError_DisallowedScheme value)  disallowedScheme,required TResult Function( SsrfError_TooManyRedirects value)  tooManyRedirects,}){
final _that = this;
switch (_that) {
case SsrfError_DeniedByPolicy():
return deniedByPolicy(_that);case SsrfError_NotOnAllowlist():
return notOnAllowlist(_that);case SsrfError_DnsResolutionFailed():
return dnsResolutionFailed(_that);case SsrfError_InvalidUrl():
return invalidUrl(_that);case SsrfError_DisallowedScheme():
return disallowedScheme(_that);case SsrfError_TooManyRedirects():
return tooManyRedirects(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( SsrfError_DeniedByPolicy value)?  deniedByPolicy,TResult? Function( SsrfError_NotOnAllowlist value)?  notOnAllowlist,TResult? Function( SsrfError_DnsResolutionFailed value)?  dnsResolutionFailed,TResult? Function( SsrfError_InvalidUrl value)?  invalidUrl,TResult? Function( SsrfError_DisallowedScheme value)?  disallowedScheme,TResult? Function( SsrfError_TooManyRedirects value)?  tooManyRedirects,}){
final _that = this;
switch (_that) {
case SsrfError_DeniedByPolicy() when deniedByPolicy != null:
return deniedByPolicy(_that);case SsrfError_NotOnAllowlist() when notOnAllowlist != null:
return notOnAllowlist(_that);case SsrfError_DnsResolutionFailed() when dnsResolutionFailed != null:
return dnsResolutionFailed(_that);case SsrfError_InvalidUrl() when invalidUrl != null:
return invalidUrl(_that);case SsrfError_DisallowedScheme() when disallowedScheme != null:
return disallowedScheme(_that);case SsrfError_TooManyRedirects() when tooManyRedirects != null:
return tooManyRedirects(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function( String reason)?  deniedByPolicy,TResult Function()?  notOnAllowlist,TResult Function( String field0)?  dnsResolutionFailed,TResult Function( String field0)?  invalidUrl,TResult Function( String field0)?  disallowedScheme,TResult Function()?  tooManyRedirects,required TResult orElse(),}) {final _that = this;
switch (_that) {
case SsrfError_DeniedByPolicy() when deniedByPolicy != null:
return deniedByPolicy(_that.reason);case SsrfError_NotOnAllowlist() when notOnAllowlist != null:
return notOnAllowlist();case SsrfError_DnsResolutionFailed() when dnsResolutionFailed != null:
return dnsResolutionFailed(_that.field0);case SsrfError_InvalidUrl() when invalidUrl != null:
return invalidUrl(_that.field0);case SsrfError_DisallowedScheme() when disallowedScheme != null:
return disallowedScheme(_that.field0);case SsrfError_TooManyRedirects() when tooManyRedirects != null:
return tooManyRedirects();case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function( String reason)  deniedByPolicy,required TResult Function()  notOnAllowlist,required TResult Function( String field0)  dnsResolutionFailed,required TResult Function( String field0)  invalidUrl,required TResult Function( String field0)  disallowedScheme,required TResult Function()  tooManyRedirects,}) {final _that = this;
switch (_that) {
case SsrfError_DeniedByPolicy():
return deniedByPolicy(_that.reason);case SsrfError_NotOnAllowlist():
return notOnAllowlist();case SsrfError_DnsResolutionFailed():
return dnsResolutionFailed(_that.field0);case SsrfError_InvalidUrl():
return invalidUrl(_that.field0);case SsrfError_DisallowedScheme():
return disallowedScheme(_that.field0);case SsrfError_TooManyRedirects():
return tooManyRedirects();}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function( String reason)?  deniedByPolicy,TResult? Function()?  notOnAllowlist,TResult? Function( String field0)?  dnsResolutionFailed,TResult? Function( String field0)?  invalidUrl,TResult? Function( String field0)?  disallowedScheme,TResult? Function()?  tooManyRedirects,}) {final _that = this;
switch (_that) {
case SsrfError_DeniedByPolicy() when deniedByPolicy != null:
return deniedByPolicy(_that.reason);case SsrfError_NotOnAllowlist() when notOnAllowlist != null:
return notOnAllowlist();case SsrfError_DnsResolutionFailed() when dnsResolutionFailed != null:
return dnsResolutionFailed(_that.field0);case SsrfError_InvalidUrl() when invalidUrl != null:
return invalidUrl(_that.field0);case SsrfError_DisallowedScheme() when disallowedScheme != null:
return disallowedScheme(_that.field0);case SsrfError_TooManyRedirects() when tooManyRedirects != null:
return tooManyRedirects();case _:
  return null;

}
}

}

/// @nodoc


class SsrfError_DeniedByPolicy extends SsrfError {
  const SsrfError_DeniedByPolicy({required this.reason}): super._();
  

 final  String reason;

/// Create a copy of SsrfError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$SsrfError_DeniedByPolicyCopyWith<SsrfError_DeniedByPolicy> get copyWith => _$SsrfError_DeniedByPolicyCopyWithImpl<SsrfError_DeniedByPolicy>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is SsrfError_DeniedByPolicy&&(identical(other.reason, reason) || other.reason == reason));
}


@override
int get hashCode => Object.hash(runtimeType,reason);

@override
String toString() {
  return 'SsrfError.deniedByPolicy(reason: $reason)';
}


}

/// @nodoc
abstract mixin class $SsrfError_DeniedByPolicyCopyWith<$Res> implements $SsrfErrorCopyWith<$Res> {
  factory $SsrfError_DeniedByPolicyCopyWith(SsrfError_DeniedByPolicy value, $Res Function(SsrfError_DeniedByPolicy) _then) = _$SsrfError_DeniedByPolicyCopyWithImpl;
@useResult
$Res call({
 String reason
});




}
/// @nodoc
class _$SsrfError_DeniedByPolicyCopyWithImpl<$Res>
    implements $SsrfError_DeniedByPolicyCopyWith<$Res> {
  _$SsrfError_DeniedByPolicyCopyWithImpl(this._self, this._then);

  final SsrfError_DeniedByPolicy _self;
  final $Res Function(SsrfError_DeniedByPolicy) _then;

/// Create a copy of SsrfError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? reason = null,}) {
  return _then(SsrfError_DeniedByPolicy(
reason: null == reason ? _self.reason : reason // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class SsrfError_NotOnAllowlist extends SsrfError {
  const SsrfError_NotOnAllowlist(): super._();
  






@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is SsrfError_NotOnAllowlist);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
  return 'SsrfError.notOnAllowlist()';
}


}




/// @nodoc


class SsrfError_DnsResolutionFailed extends SsrfError {
  const SsrfError_DnsResolutionFailed({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of SsrfError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$SsrfError_DnsResolutionFailedCopyWith<SsrfError_DnsResolutionFailed> get copyWith => _$SsrfError_DnsResolutionFailedCopyWithImpl<SsrfError_DnsResolutionFailed>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is SsrfError_DnsResolutionFailed&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'SsrfError.dnsResolutionFailed(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $SsrfError_DnsResolutionFailedCopyWith<$Res> implements $SsrfErrorCopyWith<$Res> {
  factory $SsrfError_DnsResolutionFailedCopyWith(SsrfError_DnsResolutionFailed value, $Res Function(SsrfError_DnsResolutionFailed) _then) = _$SsrfError_DnsResolutionFailedCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$SsrfError_DnsResolutionFailedCopyWithImpl<$Res>
    implements $SsrfError_DnsResolutionFailedCopyWith<$Res> {
  _$SsrfError_DnsResolutionFailedCopyWithImpl(this._self, this._then);

  final SsrfError_DnsResolutionFailed _self;
  final $Res Function(SsrfError_DnsResolutionFailed) _then;

/// Create a copy of SsrfError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(SsrfError_DnsResolutionFailed(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class SsrfError_InvalidUrl extends SsrfError {
  const SsrfError_InvalidUrl({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of SsrfError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$SsrfError_InvalidUrlCopyWith<SsrfError_InvalidUrl> get copyWith => _$SsrfError_InvalidUrlCopyWithImpl<SsrfError_InvalidUrl>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is SsrfError_InvalidUrl&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'SsrfError.invalidUrl(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $SsrfError_InvalidUrlCopyWith<$Res> implements $SsrfErrorCopyWith<$Res> {
  factory $SsrfError_InvalidUrlCopyWith(SsrfError_InvalidUrl value, $Res Function(SsrfError_InvalidUrl) _then) = _$SsrfError_InvalidUrlCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$SsrfError_InvalidUrlCopyWithImpl<$Res>
    implements $SsrfError_InvalidUrlCopyWith<$Res> {
  _$SsrfError_InvalidUrlCopyWithImpl(this._self, this._then);

  final SsrfError_InvalidUrl _self;
  final $Res Function(SsrfError_InvalidUrl) _then;

/// Create a copy of SsrfError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(SsrfError_InvalidUrl(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class SsrfError_DisallowedScheme extends SsrfError {
  const SsrfError_DisallowedScheme({required this.field0}): super._();
  

 final  String field0;

/// Create a copy of SsrfError
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$SsrfError_DisallowedSchemeCopyWith<SsrfError_DisallowedScheme> get copyWith => _$SsrfError_DisallowedSchemeCopyWithImpl<SsrfError_DisallowedScheme>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is SsrfError_DisallowedScheme&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'SsrfError.disallowedScheme(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $SsrfError_DisallowedSchemeCopyWith<$Res> implements $SsrfErrorCopyWith<$Res> {
  factory $SsrfError_DisallowedSchemeCopyWith(SsrfError_DisallowedScheme value, $Res Function(SsrfError_DisallowedScheme) _then) = _$SsrfError_DisallowedSchemeCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$SsrfError_DisallowedSchemeCopyWithImpl<$Res>
    implements $SsrfError_DisallowedSchemeCopyWith<$Res> {
  _$SsrfError_DisallowedSchemeCopyWithImpl(this._self, this._then);

  final SsrfError_DisallowedScheme _self;
  final $Res Function(SsrfError_DisallowedScheme) _then;

/// Create a copy of SsrfError
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(SsrfError_DisallowedScheme(
field0: null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class SsrfError_TooManyRedirects extends SsrfError {
  const SsrfError_TooManyRedirects(): super._();
  






@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is SsrfError_TooManyRedirects);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
  return 'SsrfError.tooManyRedirects()';
}


}




// dart format on
